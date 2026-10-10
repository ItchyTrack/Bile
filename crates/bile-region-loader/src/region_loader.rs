use std::collections::HashMap;

use bevy::ecs::{component::Component, entity::Entity};
use bile_math::Region;

use crate::lod_bands::LodBand;
use crate::tile_lifecycle::{TileLifecycle, TileResolution};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverageDebugState {
	Pending,
	Loaded,
	Empty,
	Waiting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoverageDebugTile {
	pub grid: Entity,
	pub region: Region,
	pub lod: u8,
	pub state: CoverageDebugState,
}

#[derive(Component, Default, Debug)]
pub struct RegionLoader {
	pub(crate) regions: HashMap<GridId, Vec<LodBand>>,
	pub(crate) tiles: TileLifecycle,
}

impl RegionLoader {
	pub fn tiles_to_render(&self) -> impl Iterator<Item = Entity> + '_ { self.tiles.tiles_to_render() }
	pub fn coverage_debug_tiles(&self) -> Vec<CoverageDebugTile> {
		let mut states = HashMap::new();
		for (key, _, retained) in self.tiles.coverage_debug_tiles() {
			states.insert(key, if retained { CoverageDebugState::Waiting } else { CoverageDebugState::Pending });
		}
		for (key, entry) in self.tiles.entries() {
			let state = if !self.tiles.contains_desired(key) {
				CoverageDebugState::Waiting
			} else {
				match &entry.resolution {
					TileResolution::Requested => CoverageDebugState::Pending,
					TileResolution::Empty => CoverageDebugState::Empty,
					TileResolution::Tile(_) => CoverageDebugState::Loaded,
				}
			};
			states.entry(key).and_modify(|current| {
				if !matches!(current, CoverageDebugState::Waiting) { *current = state; }
			}).or_insert(state);
		}
		let mut tiles: Vec<_> = states
			.into_iter()
			.map(|(key, state)| CoverageDebugTile { grid: key.grid, region: key.tile_key.region.into(), lod: key.tile_key.lod, state })
			.collect();
		tiles.sort_by_key(|tile| {
			let min = tile.region.min();
			(tile.grid.to_bits(), tile.lod, min.x, min.y, min.z)
		});
		tiles
	}
}
