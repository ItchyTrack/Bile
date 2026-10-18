use bevy::{ecs::component::ComponentId, prelude::*};
use bile_math::NonZeroRegion;
use bile_scene::NodeBounds;
use bile_tiles::{TileInstances, TileKey};

use crate::lod_bands::{for_each_tile_in_bands, has_duplicate_bands, run_over_diff, LodBand};
use crate::tile_lifecycle::{TileLifecycle, TileResolution};
use crate::TileClassKey;

/// Requests tiles on a [`TileInstances`] so that a set of [`LodBand`]s is covered, each with its own class.
///
/// When the bands change, tiles that are no longer wanted keep their class until every tile
/// replacing them has loaded, so moving between lods or classes never leaves a hole.
/// A tile counts as loaded once [`RegionLoader::tile_class_ready`] is called for it, which is
/// normally driven by [`bile_tiles::TileClassReady`] messages.
///
/// This is plain data. [`crate::RegionLoaderComponent`] and [`crate::RegionLoaderPlugin`] wrap it
/// for the common case of one loader component per entity.
#[derive(Debug)]
pub struct RegionLoader {
	tile_size: u32,
	bands: Vec<LodBand>,
	new_bands: Option<Vec<LodBand>>,
	tiles: TileLifecycle,
	acquire: Vec<TileClassKey>,
	release: Vec<TileClassKey>,
}

impl RegionLoader {
	/// `tile_size` is the size of a lod 0 tile in region units. Lod `n` tiles are `tile_size << n` wide.
	pub fn new(tile_size: u32) -> Self {
		assert!(tile_size > 0, "tile_size must be nonzero");
		Self { tile_size, bands: Vec::new(), new_bands: None, tiles: default(), acquire: Vec::new(), release: Vec::new() }
	}

	pub fn tile_size(&self) -> u32 { self.tile_size }
	pub fn lod_tile_size(&self, lod: u8) -> u32 { self.tile_size << lod }

	/// The bands that have been applied to the [`TileInstances`].
	pub fn regions(&self) -> &[LodBand] { &self.bands }
	/// The bands that will be applied by the next [`RegionLoader::update`], if they changed.
	pub fn pending_regions(&self) -> Option<&[LodBand]> { self.new_bands.as_deref() }
	pub fn has_pending_regions(&self) -> bool { self.new_bands.is_some() }

	/// Sets the bands to load. They are applied by the next [`RegionLoader::update`]. Each class may
	/// have at most one band per lod.
	pub fn set_regions(&mut self, bands: Vec<LodBand>) {
		assert!(!has_duplicate_bands(&bands), "a RegionLoader can have at most one band per class and lod");
		if self.new_bands.as_ref().unwrap_or(&self.bands) == &bands {
			return;
		}
		self.new_bands = Some(bands);
	}

	/// Applies pending bands by adding and removing classes on `tile_instances`.
	pub fn update(&mut self, tile_instances: &mut TileInstances, node_bounds: &NodeBounds) {
		let Some(new_bands) = self.new_bands.take() else { return };
		let mut added = Vec::new();
		let mut removed = Vec::new();
		run_over_diff(&self.bands, &new_bands, self.tile_size, node_bounds, |key, is_added| {
			if is_added { added.push(key); } else { removed.push(key); }
		});
		self.bands = new_bands;
		self.apply_delta(tile_instances, &added, &removed);
	}

	/// Call when `class` on the tile at `tile_key` has loaded. Unknown or already loaded tiles are ignored.
	pub fn tile_class_ready(&mut self, tile_instances: &mut TileInstances, tile_key: TileKey, class: ComponentId, tile_entity: Entity) {
		let released = self.tiles.resolve(TileClassKey { tile_key, class }, tile_entity);
		for key in released {
			tile_instances.remove_class(key.tile_key, key.class);
		}
	}

	/// Call when what exists inside `region` may have changed, for example because the
	/// [`NodeBounds`] grew or shrank there. Requests tiles that now overlap the bounds and
	/// releases tiles that no longer do.
	pub fn region_updated(&mut self, tile_instances: &mut TileInstances, node_bounds: &NodeBounds, region: NonZeroRegion) {
		self.update(tile_instances, node_bounds);
		let mut added = Vec::new();
		for_each_tile_in_bands(&self.bands, self.tile_size, region, node_bounds, |key| {
			if !self.tiles.contains_desired(key) { added.push(key); }
		});
		let mut removed = Vec::new();
		self.tiles.desired_in_area(region, &mut removed);
		removed.retain(|key| !node_bounds.overlapping(key.tile_key.region));
		self.apply_delta(tile_instances, &added, &removed);
	}

	/// Releases every tile and forgets the bands.
	pub fn clear(&mut self, tile_instances: &mut TileInstances) {
		for (key, _) in self.tiles.entries() {
			tile_instances.remove_class(key.tile_key, key.class);
		}
		self.tiles = default();
		self.bands.clear();
		self.new_bands = None;
	}

	/// Tiles that are loaded and currently shown, including tiles kept until their replacements load.
	pub fn loaded_tiles(&self) -> impl Iterator<Item = (TileClassKey, Entity)> + '_ {
		self.tiles.entries().filter_map(|(key, resolution)| match resolution {
			TileResolution::Loaded(entity) => Some((key, entity)),
			TileResolution::Requested => None,
		})
	}
	/// Tiles that have been requested but are not loaded yet.
	pub fn pending_tiles(&self) -> impl Iterator<Item = TileClassKey> + '_ {
		self.tiles.entries().filter_map(|(key, resolution)| resolution.is_requested().then_some(key))
	}
	pub fn is_loaded(&self, key: TileClassKey) -> bool {
		matches!(self.tiles.resolution(key), Some(TileResolution::Loaded(_)))
	}
	pub fn is_desired(&self, key: TileClassKey) -> bool { self.tiles.contains_desired(key) }
	/// The number of tiles the current bands want.
	pub fn desired_count(&self) -> usize { self.tiles.desired_len() }

	fn apply_delta(&mut self, tile_instances: &mut TileInstances, added: &[TileClassKey], removed: &[TileClassKey]) {
		if added.is_empty() && removed.is_empty() {
			return;
		}
		self.tiles.apply_delta(added, removed, &mut self.acquire, &mut self.release);
		for key in self.release.drain(..) {
			tile_instances.remove_class(key.tile_key, key.class);
		}
		for key in self.acquire.drain(..) {
			tile_instances.add_class(key.tile_key, key.class);
		}
	}
}
