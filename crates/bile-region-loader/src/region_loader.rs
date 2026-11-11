use bevy::prelude::*;
use bile_math::NonZeroRegion;
use bile_scene::NodeBounds;
use bile_tiles::TileKey;

use crate::lod_bands::{for_each_tile_in_bands, has_duplicate_bands, run_over_diff, LodBand};
use crate::tile_lifecycle::{TileLifecycle, TileResolution};

#[derive(Debug)]
pub struct RegionLoader {
	tile_size: u32,
	bands: Vec<LodBand>,
	new_bands: Option<Vec<LodBand>>,
	tiles: TileLifecycle,
	acquire: Vec<TileKey>,
	release: Vec<(TileKey, TileResolution)>,
}

impl RegionLoader {
	pub fn new(tile_size: u32) -> Self {
		assert!(tile_size > 0, "tile_size must be nonzero");
		Self { tile_size, bands: Vec::new(), new_bands: None, tiles: default(), acquire: Vec::new(), release: Vec::new() }
	}

	pub fn tile_size(&self) -> u32 { self.tile_size }
	pub fn lod_tile_size(&self, lod: u8) -> u32 { self.tile_size << lod }

	pub fn regions(&self) -> &[LodBand] { &self.bands }
	pub fn pending_regions(&self) -> Option<&[LodBand]> { self.new_bands.as_deref() }
	pub fn has_pending_regions(&self) -> bool { self.new_bands.is_some() }

	pub fn set_regions(&mut self, bands: Vec<LodBand>) {
		assert!(!has_duplicate_bands(&bands), "a RegionLoader can have at most one band per lod");
		if self.new_bands.as_ref().unwrap_or(&self.bands) == &bands {
			return;
		}
		self.new_bands = Some(bands);
	}

	pub fn update(&mut self, node_bounds: &NodeBounds) {
		let Some(new_bands) = self.new_bands.take() else { return };
		let mut added = Vec::new();
		let mut removed = Vec::new();
		run_over_diff(&self.bands, &new_bands, self.tile_size, node_bounds, |key, is_added| {
			if is_added { added.push(key); } else { removed.push(key); }
		});
		self.bands = new_bands;
		self.apply_delta(&added, &removed);
	}

	pub fn tile_loaded(&mut self, tile_key: TileKey, tile_entity: Option<Entity>) {
		self.tiles.resolve(tile_key, tile_entity, &mut self.release);
	}

	pub fn region_updated(&mut self, node_bounds: &NodeBounds, region: NonZeroRegion) {
		self.update(node_bounds);
		let mut added = Vec::new();
		for_each_tile_in_bands(&self.bands, self.tile_size, region, node_bounds, |key| {
			if !self.tiles.contains_desired(key) { added.push(key); }
		});
		let mut removed = Vec::new();
		self.tiles.desired_in_area(region, &mut removed);
		removed.retain(|key| !node_bounds.overlapping(key.region));
		self.apply_delta(&added, &removed);
	}

	pub fn clear(&mut self) {
		self.acquire.clear();
		self.tiles.take_all(&mut self.release);
		self.bands.clear();
		self.new_bands = None;
	}

	pub fn loaded_tiles(&self) -> impl Iterator<Item = (TileKey, Option<Entity>)> + '_ {
		self.tiles.entries().filter_map(|(key, resolution)| match resolution {
			TileResolution::Loaded(entity) => Some((key, entity)),
			TileResolution::Requested => None,
		})
	}
	pub fn pending_tiles(&self) -> impl Iterator<Item = TileKey> + '_ {
		self.tiles.entries().filter_map(|(key, resolution)| resolution.is_requested().then_some(key))
	}
	pub fn is_loaded(&self, key: TileKey) -> bool {
		matches!(self.tiles.resolution(key), Some(TileResolution::Loaded(_)))
	}
	pub fn is_requested(&self, key: TileKey) -> bool {
		self.tiles.resolution(key).is_some_and(|resolution| resolution.is_requested())
	}
	pub fn is_desired(&self, key: TileKey) -> bool { self.tiles.contains_desired(key) }
	pub fn desired_count(&self) -> usize { self.tiles.desired_len() }

	pub(crate) fn take_acquired(&mut self) -> Vec<TileKey> { std::mem::take(&mut self.acquire) }
	pub(crate) fn take_released(&mut self) -> Vec<(TileKey, TileResolution)> { std::mem::take(&mut self.release) }

	fn apply_delta(&mut self, added: &[TileKey], removed: &[TileKey]) {
		if added.is_empty() && removed.is_empty() {
			return;
		}
		self.tiles.apply_delta(added, removed, &mut self.acquire, &mut self.release);
	}
}
