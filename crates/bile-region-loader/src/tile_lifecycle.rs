use std::collections::{hash_map::Entry, HashMap, HashSet};

use bevy::prelude::*;
use bile_math::NonZeroRegion;
use bile_tiles::{TileIndex, TileKey};

use crate::coverage::Coverage;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TileResolution {
	Requested,
	Loaded(Option<Entity>),
}

impl TileResolution {
	pub(crate) fn is_requested(&self) -> bool { matches!(self, Self::Requested) }
	pub(crate) fn entity(&self) -> Option<Entity> {
		match self {
			Self::Loaded(entity) => *entity,
			Self::Requested => None,
		}
	}
}

#[derive(Debug, Default)]
pub(crate) struct TileLifecycle {
	desired: HashSet<TileKey>,
	desired_index: TileIndex<TileKey>,
	entries: HashMap<TileKey, TileResolution>,
	coverage: Coverage,
}

impl TileLifecycle {
	/// Applies one complete desired-set change. Releases are finalized only after every added key
	/// is registered, so an early addition cannot release a key added later in the same batch.
	pub(crate) fn apply_delta(
		&mut self, added: &[TileKey], removed: &[TileKey], acquire: &mut Vec<TileKey>, release: &mut Vec<(TileKey, TileResolution)>,
	) {
		let mut releasable = Vec::new();
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
				releasable.extend(self.coverage.set_resolved(key));
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

			releasable.extend(self.coverage.set_unwanted(key));
		}

		self.remove_releasable(releasable, release);
	}

	pub(crate) fn resolve(&mut self, key: TileKey, tile_entity: Option<Entity>, release: &mut Vec<(TileKey, TileResolution)>) {
		let Some(resolution) = self.entries.get_mut(&key) else { return };
		if !resolution.is_requested() {
			return;
		}
		*resolution = TileResolution::Loaded(tile_entity);

		let mut releasable = self.coverage.set_resolved(key);
		if !self.desired.contains(&key) {
			releasable.extend(self.coverage.set_unwanted(key));
		}
		self.remove_releasable(releasable, release);
	}

	pub(crate) fn take_all(&mut self, release: &mut Vec<(TileKey, TileResolution)>) {
		release.extend(self.entries.drain());
		*self = default();
	}

	pub(crate) fn contains_desired(&self, key: TileKey) -> bool { self.desired.contains(&key) }
	pub(crate) fn desired_len(&self) -> usize { self.desired.len() }
	pub(crate) fn entries(&self) -> impl Iterator<Item = (TileKey, TileResolution)> + '_ {
		self.entries.iter().map(|(&key, &resolution)| (key, resolution))
	}
	pub(crate) fn resolution(&self, key: TileKey) -> Option<TileResolution> { self.entries.get(&key).copied() }

	pub(crate) fn desired_in_area(&self, region: NonZeroRegion, out: &mut Vec<TileKey>) {
		out.clear();
		self.desired_index.for_each_overlapping(region, |key| out.push(key));
	}

	fn remove_releasable(&mut self, mut tile_keys: Vec<TileKey>, release: &mut Vec<(TileKey, TileResolution)>) {
		tile_keys.sort_by_key(|key| {
			let min = key.region.min();
			(key.lod, min.x, min.y, min.z)
		});
		tile_keys.dedup();
		for key in tile_keys {
			if self.desired.contains(&key) {
				continue;
			}
			if let Some(resolution) = self.entries.remove(&key) {
				release.push((key, resolution));
			}
		}
	}
}
