use bevy::{ecs::component::ComponentId, prelude::*};
use bile_math::{Region, NonZeroRegion};
use bile_scene::NodeBounds;

use crate::TileClassKey;

/// Tiles of `lod` with `class` covering `outer` minus `inner`. Both boxes should be aligned to the
/// tile size of `lod`. A loader can have at most one band per class and lod.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LodBand {
	pub class: ComponentId,
	pub lod: u8,
	pub outer: Region,
	pub inner: Option<Region>,
}

/// Runs `f(key)` for every tile that lies within a band and overlaps both `area_region` and the node bounds.
pub(crate) fn for_each_tile_in_bands(
	bands: &[LodBand],
	tile_size: u32,
	area_region: NonZeroRegion,
	node_bounds: &NodeBounds,
	mut f: impl FnMut(TileClassKey),
) {
	let area_min = area_region.min();
	let area_max = area_region.end();
	for band in bands {
		let band_tile_size = (tile_size << band.lod) as i32;
		let overlap_min = area_min.max(band.outer.min());
		let overlap_max = area_max.min(band.outer.end());
		if overlap_min.cmpge(overlap_max).any() {
			continue;
		}
		let mut x = overlap_min.x.div_euclid(band_tile_size) * band_tile_size;
		while x < overlap_max.x {
			let mut y = overlap_min.y.div_euclid(band_tile_size) * band_tile_size;
			while y < overlap_max.y {
				let mut z = overlap_min.z.div_euclid(band_tile_size) * band_tile_size;
				while z < overlap_max.z {
					let key = TileClassKey::new(tile_size, band.class, band.lod, IVec3::new(x, y, z));
					if is_tile_in_band(*band, key) && node_bounds.overlapping(key.tile_key.region) {
						f(key);
					}
					z += band_tile_size;
				}
				y += band_tile_size;
			}
			x += band_tile_size;
		}
	}
}

fn is_tile_in_band(band: LodBand, key: TileClassKey) -> bool {
	let region = Region::from(key.tile_key.region);
	band.outer.contains_region(region) && band.inner.is_none_or(|inner| !inner.contains_region(region))
}

/// Runs `f(key, is_added)` for every tile that is in `new_bands` but not `old_bands` and the reverse.
pub(crate) fn run_over_diff(
	old_bands: &[LodBand],
	new_bands: &[LodBand],
	tile_size: u32,
	node_bounds: &NodeBounds,
	mut f: impl FnMut(TileClassKey, bool),
) {
	let mut classes: Vec<ComponentId> = old_bands.iter().chain(new_bands.iter()).map(|band| band.class).collect();
	classes.sort();
	classes.dedup();
	let Some(max_lod) = old_bands.iter().chain(new_bands.iter()).map(|band| band.lod).max() else { return };

	for class in classes {
		for lod in 0..=max_lod {
			let (old_outer, old_inner) = band_boxes(old_bands, class, lod);
			let (new_outer, new_inner) = band_boxes(new_bands, class, lod);
			if new_outer == old_outer && new_inner == old_inner { continue; }
			let mut emit = |region: Region, added: bool| {
				let Ok(region) = region.try_into() else { return; };
				node_bounds.repeat_over_area(region, UVec3::splat(tile_size << lod), IVec3::ZERO, |min| {
					f(TileClassKey::new(tile_size, class, lod, min), added);
				});
			};

			for slab in region_minus_region(new_outer, old_outer) {
				for piece in region_minus_region(slab, new_inner) { emit(piece, true); }
			}
			for piece in region_minus_region(intersection(new_outer, old_inner), new_inner) { emit(piece, true); }

			for slab in region_minus_region(old_outer, new_outer) {
				for piece in region_minus_region(slab, old_inner) { emit(piece, false); }
			}
			for piece in region_minus_region(intersection(old_outer, new_inner), old_inner) { emit(piece, false); }
		}
	}
}

/// If any class has more than one band for the same lod.
pub(crate) fn has_duplicate_bands(bands: &[LodBand]) -> bool {
	bands.iter().enumerate().any(|(i, a)| bands[..i].iter().any(|b| a.class == b.class && a.lod == b.lod))
}

fn empty_region() -> Region {
	Region::new(IVec3::ZERO, UVec3::ZERO)
}

fn intersection(a: Region, b: Region) -> Region {
	a.intersection(b).map(Into::into).unwrap_or_else(empty_region)
}

fn band_boxes(bands: &[LodBand], class: ComponentId, lod: u8) -> (Region, Region) {
	match bands.iter().find(|band| band.class == class && band.lod == lod) {
		Some(band) => (band.outer, band.inner.unwrap_or_else(empty_region)),
		None => (empty_region(), empty_region()),
	}
}

fn region_minus_region(a: Region, b: Region) -> [Region; 6] {
	if a.is_empty() {
		return [empty_region(); 6];
	}
	let Some(i) = a.intersection(b) else {
		return [a, empty_region(), empty_region(), empty_region(), empty_region(), empty_region()];
	};
	[
		Region::from_min_end(a.min(), IVec3::new(i.min().x, a.end().y, a.end().z)).unwrap(),
		Region::from_min_end(IVec3::new(i.end().x, a.min().y, a.min().z), a.end()).unwrap(),
		Region::from_min_end(IVec3::new(i.min().x, a.min().y, a.min().z), IVec3::new(i.end().x, i.min().y, a.end().z)).unwrap(),
		Region::from_min_end(IVec3::new(i.min().x, i.end().y, a.min().z), IVec3::new(i.end().x, a.end().y, a.end().z)).unwrap(),
		Region::from_min_end(IVec3::new(i.min().x, i.min().y, a.min().z), IVec3::new(i.end().x, i.end().y, i.min().z)).unwrap(),
		Region::from_min_end(IVec3::new(i.min().x, i.min().y, i.end().z), IVec3::new(i.end().x, i.end().y, a.end().z)).unwrap(),
	]
}
