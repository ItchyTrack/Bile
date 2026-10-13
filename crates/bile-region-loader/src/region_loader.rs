use bevy::prelude::*;

use crate::lod_bands::LodBand;
use crate::tile_lifecycle::TileLifecycle;

#[derive(Component, Default, Debug)]
pub struct RegionLoader {
	pub(crate) regions: Vec<LodBand>,
	pub(crate) tiles: TileLifecycle,
}

impl RegionLoader {
	pub fn tiles_to_render(&self) -> impl Iterator<Item = Entity> + '_ { self.tiles.tiles_to_render() }
}
