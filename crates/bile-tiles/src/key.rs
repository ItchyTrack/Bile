use bevy::{ecs::component::Component, math::{IVec3, UVec3}};

use bile_math::NonZeroRegion;

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileKey {
	pub region: NonZeroRegion,
	pub lod: u8,
}

impl TileKey {
	pub fn new(region: NonZeroRegion, lod: u8) -> Self {
		Self { region, lod }
	}

	pub const fn min(self) -> IVec3 { self.region.min() }
	pub const fn size(self) -> UVec3 { self.region.size() }
}
