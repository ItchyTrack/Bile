use std::collections::HashMap;

use bevy::ecs::{component::Component, entity::Entity};
use bile_math::Region;

use crate::lod_bands::LodBand;
use crate::tile_lifecycle::{TileLifecycle, TileResolution};

#[derive(Component, Default, Debug)]
pub struct RegionLoader {
	pub(crate) regions: HashMap<Entity, Vec<LodBand>>,
	pub(crate) tiles: TileLifecycle,
}

impl RegionLoader {
	pub fn tiles_to_render(&self) -> impl Iterator<Item = Entity> + '_ { self.tiles.tiles_to_render() }
}
