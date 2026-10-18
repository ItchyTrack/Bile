use std::collections::{hash_map::Entry, HashMap, HashSet};

use bevy::prelude::*;
use bile_math::NonZeroRegion;
use bile_tiles::TileIndex;

use crate::{coverage::Coverage, TileClassKey};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TileResolution {
	Requested,
	Loaded(Entity),
}

impl TileResolution {
	pub(crate) fn is_requested(&self) -> bool { matches!(self, Self::Requested) }
}

/// The desired tiles of a loader, plus loaded tiles that are kept until the tiles replacing them are loaded.
#[derive(Debug, Default)]
pub(crate) struct TileLifecycle {
	desired: HashSet<TileClassKey>,
	desired_index: TileIndex<TileClassKey>,
	entries: HashMap<TileClassKey, TileResolution>,
	coverage: Coverage,
}

impl TileLifecycle {
	/// Applies one complete desired-set change using caller-owned output buffers. Releases are
	/// finalized only after every added key is registered, so an early addition cannot release a
	/// key added later in the same batch.
	pub(crate) fn apply_delta(
		&mut self, added: &[TileClassKey], removed: &[TileClassKey], acquire: &mut Vec<TileClassKey>, release: &mut Vec<TileClassKey>,
	) {
		acquire.clear();
		release.clear();

		for &key in added {
			if !self.desired.insert(key) {
				panic!("Dont add already desired tiles.");
			}
			self.desired_index.insert(key);
			if let Entry::Vacant(entry) = self.entries.entry(key) {
				entry.insert(TileResolution::Requested);
				acquire.push(key);
			}
		}

		// Establish all pending coverage before resolved reactivations satisfy dependencies. This
		// makes the entire added slice visible to coverage before it can emit a release candidate.
		for &key in added {
			self.coverage.set_wanted(key);
		}
		for &key in added {
			if !self.entries[&key].is_requested() {
				release.extend(self.coverage.set_resolved(key));
			}
		}

		for &key in removed {
			if !self.desired.remove(&key) {
				continue;
			}
			self.desired_index.remove(key);
			if !self.entries.contains_key(&key) {
				continue;
			}

			release.extend(self.coverage.set_unwanted(key));
		}

		self.remove_releasable(release);
	}

	/// Marks a requested tile as loaded. Returns tiles that are no longer needed to cover it.
	#[must_use = "released tiles must have their class removed"]
	pub(crate) fn resolve(&mut self, key: TileClassKey, tile_entity: Entity) -> Vec<TileClassKey> {
		let Some(resolution) = self.entries.get_mut(&key) else { return Vec::new() };
		if !resolution.is_requested() {
			return Vec::new();
		}
		*resolution = TileResolution::Loaded(tile_entity);

		let mut release = self.coverage.set_resolved(key);
		if !self.desired.contains(&key) {
			release.extend(self.coverage.set_unwanted(key));
		}
		self.remove_releasable(&mut release);
		release
	}

	pub(crate) fn contains_desired(&self, key: TileClassKey) -> bool { self.desired.contains(&key) }
	pub(crate) fn desired_len(&self) -> usize { self.desired.len() }
	pub(crate) fn entries(&self) -> impl Iterator<Item = (TileClassKey, TileResolution)> + '_ {
		self.entries.iter().map(|(&key, &resolution)| (key, resolution))
	}
	pub(crate) fn resolution(&self, key: TileClassKey) -> Option<TileResolution> { self.entries.get(&key).copied() }

	pub(crate) fn desired_in_area(&self, region: NonZeroRegion, out: &mut Vec<TileClassKey>) {
		out.clear();
		self.desired_index.for_each_overlapping(region, |key| out.push(key));
	}

	fn remove_releasable(&mut self, tile_keys: &mut Vec<TileClassKey>) {
		tile_keys.sort_by_key(|key| {
			let min = key.tile_key.region.min();
			(key.tile_key.lod, min.x, min.y, min.z, key.class.index())
		});
		tile_keys.dedup();
		tile_keys.retain(|key| {
			if self.desired.contains(key) {
				return false;
			}
			self.entries.remove(key).is_some()
		});
	}
}
