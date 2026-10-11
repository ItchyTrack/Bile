use bevy::prelude::*;
use bile_math::NonZeroRegion;
use bile_tiles::{TileIndexKey, TileKey};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct EntityTileKey {
	pub(crate) entity: Entity,
	pub(crate) tile_key: TileKey,
}

impl EntityTileKey {
	pub(crate) fn new(entity: Entity, lod: u8, min: IVec3) -> Self {
		let size = 1u32 << lod;
		Self {
			entity,
			tile_key: TileKey {
				lod,
				region: NonZeroRegion::new(min, UVec3::splat(size)).unwrap()
			}
		}
	}
}

impl TileIndexKey for EntityTileKey {
	fn region(self) -> NonZeroRegion { self.tile_key.region }
}
