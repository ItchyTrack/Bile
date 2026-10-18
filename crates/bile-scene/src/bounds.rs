use bevy::prelude::*;
use bile_math::NonZeroRegion;

/// The area of a node that has data. Tiles are only requested where they overlap these bounds.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeBounds {
	region: NonZeroRegion,
}

impl NodeBounds {
	pub fn new(region: NonZeroRegion) -> Self {
		Self { region }
	}

	pub fn region(&self) -> NonZeroRegion {
		self.region
	}

	/// Changing the bounds does not update existing requests by itself. Send a region updated
	/// message covering the changed area so loaders can request or release tiles in it.
	pub fn set_region(&mut self, region: NonZeroRegion) {
		self.region = region;
	}

	pub fn overlapping(&self, region: NonZeroRegion) -> bool {
		self.region.intersects(region)
	}

	pub fn repeat_over_area(&self, region: NonZeroRegion, tile_size: UVec3, tile_origin_offset: IVec3, mut f: impl FnMut(IVec3)) {
		let Some(region) = self.region.intersection(region) else { return; };
		let tile_size = tile_size.max(UVec3::ONE).as_ivec3();
		let start = (region.min() - tile_origin_offset).div_euclid(tile_size);
		let end = (region.max() - tile_origin_offset).div_euclid(tile_size);
		for x in start.x..=end.x {
			for y in start.y..=end.y {
				for z in start.z..=end.z {
					let tile_pos = tile_origin_offset + IVec3::new(x, y, z) * tile_size;
					f(tile_pos);
				}
			}
		}
	}
}
