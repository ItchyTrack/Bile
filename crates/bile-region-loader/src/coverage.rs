use std::collections::{HashMap, HashSet};

use bile_math::NonZeroRegion;

use crate::{types::EntityTileKey, unresolved_tile_index::UnresolvedTileIndex};

/// Tracks pending coverage and the loaded tiles retained until that coverage resolves.
#[derive(Clone, Debug, Default)]
pub(crate) struct Coverage {
	pending: UnresolvedTileIndex,
	replacements_by_source: HashMap<EntityTileKey, HashSet<EntityTileKey>>,
	sources_by_replacement: HashMap<EntityTileKey, HashSet<EntityTileKey>>,
}

impl Coverage {
	pub(crate) fn debug_tiles(&self) -> Vec<(EntityTileKey, bool, bool)> {
		let mut roles: HashMap<EntityTileKey, (bool, bool)> = HashMap::new();
		for key in self.pending.keys() {
			roles.entry(key).or_default().0 = true;
		}
		for &key in self.replacements_by_source.keys() {
			roles.entry(key).or_default().1 = true;
		}
		let mut tiles: Vec<_> = roles.into_iter().map(|(key, (pending, retained))| (key, pending, retained)).collect();
		tiles.sort_by_key(|(key, _, _)| {
			let min = key.tile_key.region.min();
			(key.entity.to_bits(), key.tile_key.lod, min.x, min.y, min.z)
		});
		tiles
	}

	/// Registers coverage that is now wanted. If the tile was previously being retained, reverse
	/// that handoff so its former replacements can be removed once this tile is available again.
	pub(crate) fn set_wanted(&mut self, key: EntityTileKey) {
		if self.pending.contains(key) {
			return;
		}
		self.pending.insert(key);

		if let Some(replacements) = self.detach_source(key) {
			for replacement in replacements {
				self.add_dependency(replacement, key);
			}
		}
		let overlapping_sources: Vec<_> = self
			.replacements_by_source
			.keys()
			.copied()
			.filter(|source| tiles_overlap(*source, key))
			.collect();
		for source in overlapping_sources {
			self.add_dependency(source, key);
		}
	}

	/// Marks wanted coverage as resolved (visible or empty) and returns retained tiles that it now safely replaces.
	#[must_use = "tiles returned by Coverage must be removed from loader, render, and streaming state"]
	pub(crate) fn set_resolved(&mut self, key: EntityTileKey) -> Vec<EntityTileKey> {
		self.pending.remove(key);
		self.apply_satisfied(key)
	}

	/// Removes a tile from the wanted set and returns tiles whose requests or coverage can now be
	/// removed without opening a hole.
	#[must_use = "tiles returned by Coverage must be removed from loader, render, and streaming state"]
	pub(crate) fn set_unwanted(&mut self, key: EntityTileKey) -> Vec<EntityTileKey> {
		if self.pending.remove(key) {
			let mut removable = self.apply_satisfied(key);
			if !self.replacements_by_source.contains_key(&key) {
				removable.push(key);
			}
			return self.only_unwanted(removable);
		}

		if self.replacements_by_source.contains_key(&key) {
			return Vec::new();
		}

		let mut replacements = HashSet::new();
		if let Some(region) = NonZeroRegion::new(key.tile_key.region.min(), key.tile_key.region.size()) {
			self.pending.for_each_in_region(key.entity, region, Some(key.tile_key.lod), |candidate| {
				replacements.insert(candidate);
			});
		}
		if replacements.is_empty() {
			vec![key]
		} else {
			for replacement in replacements {
				self.add_dependency(key, replacement);
			}
			Vec::new()
		}
	}

	fn add_dependency(&mut self, source: EntityTileKey, replacement: EntityTileKey) {
		if source == replacement {
			return;
		}
		if self.replacements_by_source.entry(source).or_default().insert(replacement) {
			self.sources_by_replacement.entry(replacement).or_default().insert(source);
		}
	}

	fn apply_satisfied(&mut self, key: EntityTileKey) -> Vec<EntityTileKey> {
		let mut satisfied = HashSet::from([key]);
		let mut pending = vec![key];
		let mut removable = Vec::new();

		while let Some(replacement) = pending.pop() {
			let Some(sources) = self.sources_by_replacement.get(&replacement) else { continue };
			for source in sources {
				if satisfied.contains(source) {
					continue;
				}
				let Some(replacements) = self.replacements_by_source.get(source) else { continue };
				if replacements.iter().all(|replacement| satisfied.contains(replacement)) {
					satisfied.insert(*source);
					pending.push(*source);
					removable.push(*source);
				}
			}
		}

		for replacement in &satisfied {
			let Some(sources) = self.sources_by_replacement.remove(replacement) else { continue };
			for source in sources {
				if let Some(replacements) = self.replacements_by_source.get_mut(&source) {
					replacements.remove(replacement);
				}
			}
		}
		for source in &removable {
			self.replacements_by_source.remove(source);
		}
		self.only_unwanted(removable)
	}

	fn detach_source(&mut self, source: EntityTileKey) -> Option<HashSet<EntityTileKey>> {
		let replacements = self.replacements_by_source.remove(&source)?;
		for replacement in &replacements {
			let Some(sources) = self.sources_by_replacement.get_mut(replacement) else { continue };
			sources.remove(&source);
			if sources.is_empty() {
				self.sources_by_replacement.remove(replacement);
			}
		}
		Some(replacements)
	}

	fn only_unwanted(&self, tiles: Vec<EntityTileKey>) -> Vec<EntityTileKey> {
		let mut seen = HashSet::new();
		tiles.into_iter().filter(|key| !self.pending.contains(*key) && seen.insert(*key)).collect()
	}
}

fn tiles_overlap(a: EntityTileKey, b: EntityTileKey) -> bool {
	a.entity == b.entity
		&& a.tile_key.region.min().cmplt(b.tile_key.region.min() + b.tile_key.region.size().as_ivec3()).all()
		&& b.tile_key.region.min().cmplt(a.tile_key.region.min() + a.tile_key.region.size().as_ivec3()).all()
}
