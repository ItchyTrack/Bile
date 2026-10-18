use std::collections::{HashMap, HashSet};

use bile_tiles::TileKeySet;

use crate::TileClassKey;

/// Tracks pending coverage and the loaded tiles retained until that coverage resolves.
#[derive(Debug, Default)]
pub(crate) struct Coverage {
	pending: TileKeySet<TileClassKey>,
	replacements_by_source: HashMap<TileClassKey, HashSet<TileClassKey>>,
	sources_by_replacement: HashMap<TileClassKey, HashSet<TileClassKey>>,
}

impl Coverage {
	/// Registers coverage that is now wanted. If the tile was previously being retained, reverse
	/// that handoff so its former replacements can be removed once this tile is available again.
	pub(crate) fn set_wanted(&mut self, key: TileClassKey) {
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
			.filter(|source| source.tile_key.region.intersects(key.tile_key.region))
			.collect();
		for source in overlapping_sources {
			self.add_dependency(source, key);
		}
	}

	/// Marks wanted coverage as resolved (visible or empty) and returns retained tiles that it now safely replaces.
	#[must_use = "tiles returned by Coverage must be removed from loader, render, and streaming state"]
	pub(crate) fn set_resolved(&mut self, tile_key: TileClassKey) -> Vec<TileClassKey> {
		self.pending.remove(tile_key);
		self.apply_satisfied(tile_key)
	}

	/// Removes a tile from the wanted set and returns tiles whose requests or coverage can now be
	/// removed without opening a hole.
	#[must_use = "tiles returned by Coverage must be removed from loader, render, and streaming state"]
	pub(crate) fn set_unwanted(&mut self, tile_key: TileClassKey) -> Vec<TileClassKey> {
		if self.pending.remove(tile_key) {
			let mut removable = self.apply_satisfied(tile_key);
			if !self.replacements_by_source.contains_key(&tile_key) {
				removable.push(tile_key);
			}
			return self.only_unwanted(removable);
		}

		if self.replacements_by_source.contains_key(&tile_key) {
			return Vec::new();
		}

		let mut replacements = HashSet::new();
		self.pending.for_each_overlapping(tile_key.tile_key.region, |candidate| {
			if candidate != tile_key { replacements.insert(candidate); }
		});
		if replacements.is_empty() {
			vec![tile_key]
		} else {
			for replacement in replacements {
				self.add_dependency(tile_key, replacement);
			}
			Vec::new()
		}
	}

	fn add_dependency(&mut self, source: TileClassKey, replacement: TileClassKey) {
		if source == replacement {
			return;
		}
		if self.replacements_by_source.entry(source).or_default().insert(replacement) {
			self.sources_by_replacement.entry(replacement).or_default().insert(source);
		}
	}

	fn apply_satisfied(&mut self, key: TileClassKey) -> Vec<TileClassKey> {
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

	fn detach_source(&mut self, source: TileClassKey) -> Option<HashSet<TileClassKey>> {
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

	fn only_unwanted(&self, tiles: Vec<TileClassKey>) -> Vec<TileClassKey> {
		let mut seen = HashSet::new();
		tiles.into_iter().filter(|key| !self.pending.contains(*key) && seen.insert(*key)).collect()
	}
}
