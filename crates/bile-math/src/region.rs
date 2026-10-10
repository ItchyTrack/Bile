use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// Returned when a nonzero region is constructed with a zero-sized axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ZeroSizedRegionError;

impl std::fmt::Display for ZeroSizedRegionError {
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		formatter.write_str("region size must be nonzero on every axis")
	}
}

impl std::error::Error for ZeroSizedRegionError {}

/// A three-dimensional integer region with a signed minimum and unsigned size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Region {
	min: IVec3,
	size: UVec3,
}

impl Region {
	pub const fn new(min: IVec3, size: UVec3) -> Self {
		Self { min, size }
	}

	pub fn from_min_end(min: IVec3, end: IVec3) -> Option<Self> {
		min.cmple(end).all().then(|| Self { min, size: (end - min).as_uvec3() })
	}

	pub fn from_min_max(min: IVec3, max: IVec3) -> Option<Self> {
		Self::from_min_end(min, max + IVec3::ONE)
	}

	pub const fn from_single(pos: IVec3) -> Self {
		Self { min: pos, size: UVec3::ONE }
	}

	pub const fn min(self) -> IVec3 {
		self.min
	}

	pub const fn size(self) -> UVec3 {
		self.size
	}

	pub const fn area(self) -> u32 {
		self.size.x * self.size.y * self.size.z
	}

	pub fn end(self) -> IVec3 {
		self.min + self.size.as_ivec3()
	}

	pub const fn is_empty(self) -> bool {
		self.size.x == 0 || self.size.y == 0 || self.size.z == 0
	}

	pub fn contains(self, position: IVec3) -> bool {
		position.cmpge(self.min).all() && position.cmplt(self.end()).all()
	}

	pub fn contains_region(self, other: impl Into<Region>) -> bool {
		let other = other.into();
		other.min.cmpge(self.min).all() && other.end().cmple(self.end()).all()
	}

	pub fn intersects(self, other: impl Into<Region>) -> bool {
		let other = other.into();
		self.min.cmplt(other.end()).all() && other.min.cmplt(self.end()).all()
	}

	pub fn intersection(self, other: impl Into<Region>) -> Option<NonZeroRegion> {
		let other = other.into();
		let min = self.min.max(other.min);
		let end = self.end().min(other.end());
		NonZeroRegion::from_min_end(min, end)
	}

	pub fn translated(self, offset: IVec3) -> Self {
		Self { min: self.min + offset, size: self.size }
	}
}

/// A region whose size is nonzero on every axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "Region")]
pub struct NonZeroRegion {
	min: IVec3,
	size: UVec3,
}

impl NonZeroRegion {
	pub fn new(min: IVec3, size: UVec3) -> Option<Self> {
		size.cmpgt(UVec3::ZERO).all().then_some(Self { min, size })
	}

	pub fn from_min_end(min: IVec3, end: IVec3) -> Option<Self> {
		min.cmplt(end).all().then(|| Self { min, size: (end - min).as_uvec3() })
	}

	pub fn from_min_max(min: IVec3, max: IVec3) -> Option<Self> {
		Self::from_min_end(min, max + IVec3::ONE)
	}

	pub const fn from_single(pos: IVec3) -> Self {
		Self { min: pos, size: UVec3::ONE }
	}

	pub const fn min(self) -> IVec3 {
		self.min
	}

	pub const fn size(self) -> UVec3 {
		self.size
	}

	pub const fn area(self) -> u32 {
		self.size.x * self.size.y * self.size.z
	}

	pub fn end(self) -> IVec3 {
		self.min + self.size.as_ivec3()
	}

	pub fn max(self) -> IVec3 {
		self.end() - IVec3::ONE
	}

	pub fn contains(self, position: IVec3) -> bool {
		Region::from(self).contains(position)
	}

	pub fn contains_region(self, other: impl Into<Region>) -> bool {
		Region::from(self).contains_region(other)
	}

	pub fn intersects(self, other: impl Into<Region>) -> bool {
		Region::from(self).intersects(other)
	}

	pub fn intersection(self, other: impl Into<Region>) -> Option<NonZeroRegion> {
		Region::from(self).intersection(other)
	}

	pub fn translated(self, offset: IVec3) -> Self {
		Self { min: self.min + offset, size: self.size }
	}
}

impl TryFrom<Region> for NonZeroRegion {
	type Error = ZeroSizedRegionError;

	fn try_from(region: Region) -> Result<Self, Self::Error> {
		Self::new(region.min, region.size).ok_or(ZeroSizedRegionError)
	}
}

impl From<NonZeroRegion> for Region {
	fn from(region: NonZeroRegion) -> Self {
		Self { min: region.min, size: region.size }
	}
}
