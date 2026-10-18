use bevy::{ecs::component::ComponentId, prelude::*};
use bile_math::NonZeroRegion;
use bile_tiles::{TileIndexKey, TileKey};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileClassKey {
	pub tile_key: TileKey,
	pub class: ComponentId,
}

impl TileClassKey {
	pub fn new(tile_size: u32, class: ComponentId, lod: u8, min: IVec3) -> Self {
		let size = tile_size << lod;
		Self { tile_key: TileKey::new(NonZeroRegion::new(min, UVec3::splat(size)).unwrap(), lod), class }
	}
}

impl TileIndexKey for TileClassKey {
	fn region(self) -> NonZeroRegion { self.tile_key.region }
}
