use bevy::prelude::*;
use bile_math::NonZeroRegion;
use bile_tiles::TileKey;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct GridTileKey {
	pub(crate) grid: GridId,
	pub(crate) tile_key: TileKey,
}

impl GridTileKey {
	pub(crate) fn new(grid: GridId, lod: u8, min: IVec3) -> Self {
		let size = 1u32 << lod;
		Self {
			grid,
			tile_key: TileKey {
				lod,
				region: NonZeroRegion::new(min, UVec3::splat(size)).unwrap()
			}
		}
	}
}

impl TileIndexKey for GridTileKey {
	fn region(self) -> NonZeroRegion { self.tile_key.region }
}
