use bevy::ecs::component::ComponentId;
use bevy::prelude::*;

use crate::lod_bands::LodBand;
use crate::tile_lifecycle::TileLifecycle;

#[derive(Component, Debug)]
pub struct RegionLoader {
	pub(crate) regions: Vec<LodBand>,
	pub(crate) class: Option<ComponentId>, // None means disabled
	pub(crate) new_regions: Option<Vec<LodBand>>,
	pub(crate) new_class: Option<ComponentId>,
	pub(crate) tiles: TileLifecycle,
}

impl RegionLoader {
	pub fn tiles_to_render(&self) -> impl Iterator<Item = Entity> + '_ { self.tiles.tiles_to_render() }
}
