use std::collections::{HashMap, HashSet};

use bevy::prelude::*;

use crate::types::GridTileKey;

const PRESENT: u16 = 1;
const MAX_LOD_BITS: usize = u32::BITS as usize;
const MAX_SAFE_TILE_LOD: u8 = i32::BITS as u8 - 2;

#[derive(Debug, Clone, Default)]
pub(crate) struct UnresolvedTileIndex {
	by_grid: HashMap<(GridId, TileClassId), PerGridIndex>,
}

#[derive(Debug, Clone)]
struct PerGridIndex {
	lod_mask: u32,
	trees: [SignedGridTree<U16Cell>; MAX_LOD_BITS],
}

impl Default for PerGridIndex {
	fn default() -> Self {
		Self { lod_mask: 0, trees: std::array::from_fn(|_| SignedGridTree::new()) }
	}
}

impl UnresolvedTileIndex {
	pub(crate) fn contains(&self, key: GridTileKey) -> bool {
		key.tile_key.lod <= MAX_SAFE_TILE_LOD
			&& self.by_grid.get(&(key.grid, key.tile_key.class)).is_some_and(|grid| grid.trees[key.tile_key.lod as usize].get(key.tile_key.region.min()).is_some())
	}

	pub(crate) fn insert(&mut self, key: GridTileKey) {
		if key.tile_key.lod > MAX_SAFE_TILE_LOD {
			return;
		}
		let grid = self.by_grid.entry((key.grid, key.tile_key.class)).or_default();
		grid.trees[key.tile_key.lod as usize].add_area(NonZeroRegion::new(key.tile_key.region.min(), key.tile_key.region.size()).unwrap(), PRESENT);
		grid.lod_mask |= lod_bit(key.tile_key.lod);
	}

	pub(crate) fn remove(&mut self, key: GridTileKey) -> bool {
		if !self.contains(key) {
			return false;
		}
		let Some(grid) = self.by_grid.get_mut(&(key.grid, key.tile_key.class)) else { return false };
		grid.trees[key.tile_key.lod as usize].remove_area(NonZeroRegion::new(key.tile_key.region.min(), key.tile_key.region.size()).unwrap());
		if grid.trees[key.tile_key.lod as usize].is_empty() {
			grid.lod_mask &= !lod_bit(key.tile_key.lod);
		}
		if grid.lod_mask == 0 {
			self.by_grid.remove(&(key.grid, key.tile_key.class));
		}
		true
	}

	pub(crate) fn keys(&self) -> Vec<GridTileKey> {
		let mut keys = HashSet::new();
		for (&(grid, class), index) in &self.by_grid {
			for lod in 0..=MAX_SAFE_TILE_LOD {
				if index.lod_mask & lod_bit(lod) == 0 { continue; }
				let tile_size = 1i32 << lod;
				for (origin, region_size, _) in index.trees[lod as usize].iter() {
					let region_size = region_size as i32;
					let first = origin.div_euclid(IVec3::splat(tile_size)) * tile_size;
					let end = origin + IVec3::splat(region_size);
					for x in (first.x..end.x).step_by(tile_size as usize) {
						for y in (first.y..end.y).step_by(tile_size as usize) {
							for z in (first.z..end.z).step_by(tile_size as usize) {
								keys.insert(GridTileKey::new(grid, class, lod, IVec3::new(x, y, z)));
							}
						}
					}
				}
			}
		}
		keys.into_iter().collect()
	}

	pub(crate) fn for_each_in_region(
		&self,
		grid: GridId,
		class: TileClassId,
		region: NonZeroRegion,
		max_lod: u8,
		skip_lod: Option<u8>,
		mut f: impl FnMut(GridTileKey),
	) {
		let Some(grid_index) = self.by_grid.get(&(grid, class)) else { return };
		let mut lod_mask = grid_index.lod_mask & lod_mask_through(max_lod.min(MAX_SAFE_TILE_LOD));
		if let Some(skip_lod) = skip_lod {
			lod_mask &= !lod_bit(skip_lod);
		}
		while lod_mask != 0 {
			let lod = lod_mask.trailing_zeros() as u8;
			grid_index.trees[lod as usize].for_each_occupied_tile_cover(region, 1u32 << lod, |min| {
				f(GridTileKey::new(grid, class, lod, min));
			});
			lod_mask &= lod_mask - 1;
		}
	}
}

#[inline]
fn lod_bit(lod: u8) -> u32 {
	1u32.checked_shl(lod as u32).unwrap_or(0)
}

#[inline]
fn lod_mask_through(max_lod: u8) -> u32 {
	if max_lod as u32 >= u32::BITS - 1 {
		u32::MAX
	} else {
		(1u32 << (max_lod + 1)) - 1
	}
}
