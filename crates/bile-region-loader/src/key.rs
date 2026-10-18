use bevy::{ecs::component::ComponentId, prelude::*};
use bile_math::NonZeroRegion;
use bile_tiles::{TileIndexKey, TileKey};

/// A tile together with the class a [`crate::RegionLoader`] wants on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileClassKey {
	pub tile_key: TileKey,
	pub class: ComponentId,
}

impl TileClassKey {
	/// `tile_size` is the size of a lod 0 tile.
	pub fn new(tile_size: u32, class: ComponentId, lod: u8, min: IVec3) -> Self {
		let size = tile_size << lod;
		Self { tile_key: TileKey::new(NonZeroRegion::new(min, UVec3::splat(size)).unwrap(), lod), class }
	}
}

impl TileIndexKey for TileClassKey {
	fn region(self) -> NonZeroRegion { self.tile_key.region }
}
