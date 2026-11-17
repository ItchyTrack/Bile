use std::collections::{HashMap, HashSet, hash_map::Entry};
use std::fmt::Debug;
use std::hash::Hash;

use bevy::prelude::*;
use bile_math::NonZeroRegion;
use rustc_hash::FxHashMap;

use crate::TileKey;

pub trait TileIndexKey: Debug + Copy + Eq + Hash {
	fn region(self) -> NonZeroRegion;
}

/// Coarsest bin level. Bins at this level are `2^30` wide, the largest size that fits in an `i32`, and have no parent.
const MAX_LOD: u8 = (i32::BITS - 2) as u8;
const LOD_COUNT: usize = MAX_LOD as usize + 1;

/// One aligned cube of space: the bin at `(lod, min)`.
#[derive(Debug, Clone)]
struct Node<K> {
	/// Keys binned at this level that intersect this bin.
	keys: Vec<K>,
	/// Which of the eight child bins exist. A bin exists only while it or one of its descendants holds a key.
	children: u8,
}

impl<K> Node<K> {
	fn is_unused(&self) -> bool {
		self.keys.is_empty() && self.children == 0
	}
}

/// Spatial index over keys of any size.
///
/// A key is stored in the bins of the level matching its size, as in a multi-level grid. The bins of all levels
/// also form a sparse octree: each bin records which of its children exist, so a query over a large region
/// descends only into occupied space.
#[derive(Debug, Clone)]
pub struct TileIndex<K: TileIndexKey> {
	/// The existing bins of each level, by minimum corner.
	levels: [FxHashMap<IVec3, Node<K>>; LOD_COUNT],
	/// Number of keys binned at each level, so queries can skip unused levels.
	keys_per_lod: [u32; LOD_COUNT],
}

impl<K: TileIndexKey> Default for TileIndex<K> {
	fn default() -> Self {
		Self { levels: default(), keys_per_lod: default() }
	}
}

impl<K: TileIndexKey> TileIndex<K> {
	pub fn insert(&mut self, key: K) {
		let lod = bin_lod(key.region().size());
		let mut inserted = false;
		for_each_bin_intersecting_key(key, lod, |bin| {
			match self.levels[lod as usize].entry(bin) {
				Entry::Occupied(mut entry) => {
					let node = entry.get_mut();
					if !node.keys.contains(&key) {
						node.keys.push(key);
						inserted = true;
					}
				}
				Entry::Vacant(entry) => {
					entry.insert(Node { keys: vec![key], children: 0 });
					inserted = true;
					self.link_to_parent(lod, bin);
				}
			}
		});
		if inserted {
			self.keys_per_lod[lod as usize] += 1;
		}
	}

	pub fn remove(&mut self, key: K) {
		let lod = bin_lod(key.region().size());
		let mut removed = false;
		for_each_bin_intersecting_key(key, lod, |bin| {
			let Some(node) = self.levels[lod as usize].get_mut(&bin) else { return };
			let len = node.keys.len();
			node.keys.retain(|candidate| *candidate != key);
			removed |= node.keys.len() != len;
			if node.is_unused() {
				self.levels[lod as usize].remove(&bin);
				self.unlink_from_parent(lod, bin);
			}
		});
		if removed {
			self.keys_per_lod[lod as usize] -= 1;
		}
	}

	pub fn for_each_overlapping(&self, region: NonZeroRegion, mut f: impl FnMut(K)) {
		let region_lod = bin_lod(region.size());
		// Keys at or below the region's own level: walk down from the few bins the region covers at that level,
		// following only children that exist.
		for_each_bin_intersecting_region(region, region_lod, |bin| {
			if self.levels[region_lod as usize].contains_key(&bin) {
				self.visit_overlapping(region, region_lod, bin, &mut f);
			}
		});
		// Larger keys: the region covers at most eight bins per level.
		for lod in region_lod + 1..=MAX_LOD {
			if self.keys_per_lod[lod as usize] == 0 {
				continue;
			}
			for_each_bin_intersecting_region(region, lod, |bin| {
				if let Some(node) = self.levels[lod as usize].get(&bin) {
					report_overlapping(node, region, lod, bin, &mut f);
				}
			});
		}
	}

	pub fn keys_covering_point(&self, point: IVec3) -> Vec<K> {
		let mut out = Vec::new();
		for lod in 0..=MAX_LOD {
			if self.keys_per_lod[lod as usize] == 0 {
				continue;
			}
			let Some(node) = self.levels[lod as usize].get(&align_to_lod_bin(point, lod)) else { continue };
			out.extend(node.keys.iter().copied().filter(|key| key.region().contains(point)));
		}
		out
	}

	pub fn is_empty(&self) -> bool {
		self.levels.iter().all(FxHashMap::is_empty)
	}

	/// Reports the keys in the existing bin `(lod, bin)` and everything below it that overlap `region`.
	fn visit_overlapping(&self, region: NonZeroRegion, lod: u8, bin: IVec3, f: &mut impl FnMut(K)) {
		let node = &self.levels[lod as usize][&bin];
		report_overlapping(node, region, lod, bin, f);
		if node.children == 0 {
			return;
		}
		let child_lod = lod - 1;
		let child_size = lod_bin_size(child_lod);
		let (region_min, region_max) = (region.min(), region.max());
		for child in 0..8u8 {
			if node.children & (1 << child) == 0 {
				continue;
			}
			let child_min = bin + child_offset(child) * child_size;
			let child_max = child_min + IVec3::splat(child_size - 1);
			if child_min.cmple(region_max).all() && child_max.cmpge(region_min).all() {
				self.visit_overlapping(region, child_lod, child_min, f);
			}
		}
	}

	/// Records the new bin `(lod, bin)` in its parent, creating ancestors until one already exists.
	fn link_to_parent(&mut self, mut lod: u8, mut bin: IVec3) {
		while lod < MAX_LOD {
			let parent = align_to_lod_bin(bin, lod + 1);
			let bit = child_bit(bin, parent, lod);
			match self.levels[lod as usize + 1].entry(parent) {
				Entry::Occupied(mut entry) => {
					entry.get_mut().children |= bit;
					return;
				}
				Entry::Vacant(entry) => {
					entry.insert(Node { keys: Vec::new(), children: bit });
				}
			}
			(lod, bin) = (lod + 1, parent);
		}
	}

	/// Clears the removed bin `(lod, bin)` from its parent, removing ancestors that become unused.
	fn unlink_from_parent(&mut self, mut lod: u8, mut bin: IVec3) {
		while lod < MAX_LOD {
			let parent = align_to_lod_bin(bin, lod + 1);
			let bit = child_bit(bin, parent, lod);
			let Some(node) = self.levels[lod as usize + 1].get_mut(&parent) else { return };
			node.children &= !bit;
			if !node.is_unused() {
				return;
			}
			self.levels[lod as usize + 1].remove(&parent);
			(lod, bin) = (lod + 1, parent);
		}
	}
}

/// Reports the keys of one bin that overlap `region`.
///
/// A key can sit in several bins of its level. It is reported only from the bin holding the minimum corner of its
/// overlap with `region`, which every query visits, so each key is reported exactly once.
fn report_overlapping<K: TileIndexKey>(node: &Node<K>, region: NonZeroRegion, lod: u8, bin: IVec3, f: &mut impl FnMut(K)) {
	for &key in &node.keys {
		let key_region = key.region();
		if region.intersects(key_region) && align_to_lod_bin(key_region.min().max(region.min()), lod) == bin {
			f(key);
		}
	}
}

fn child_offset(child: u8) -> IVec3 {
	IVec3::new((child & 1) as i32, ((child >> 1) & 1) as i32, ((child >> 2) & 1) as i32)
}

fn child_bit(bin: IVec3, parent: IVec3, lod: u8) -> u8 {
	let offset = (bin - parent) >> lod;
	1 << (offset.x | (offset.y << 1) | (offset.z << 2))
}

fn for_each_bin_intersecting_key<K: TileIndexKey>(key: K, lod: u8, f: impl FnMut(IVec3)) {
	for_each_bin_intersecting_region(key.region(), lod, f);
}

fn bin_lod(size: UVec3) -> u8 {
	let max_size = size.max_element().max(1);
	(u32::BITS - (max_size - 1).leading_zeros()).min(MAX_LOD as u32) as u8
}

fn for_each_bin_intersecting_region(region: NonZeroRegion, lod: u8, mut f: impl FnMut(IVec3)) {
	let bin_size = lod_bin_size(lod);
	let start = align_to_lod_bin(region.min(), lod);
	let end = align_to_lod_bin(region.max(), lod);
	for x in (start.x..=end.x).step_by(bin_size as usize) {
		for y in (start.y..=end.y).step_by(bin_size as usize) {
			for z in (start.z..=end.z).step_by(bin_size as usize) {
				f(IVec3::new(x, y, z));
			}
		}
	}
}

fn align_to_lod_bin(pos: IVec3, lod: u8) -> IVec3 {
	let size = IVec3::splat(lod_bin_size(lod));
	pos.div_euclid(size) * size
}

fn lod_bin_size(lod: u8) -> i32 {
	1i32 << lod
}

impl TileIndexKey for TileKey {
	fn region(self) -> NonZeroRegion {
		self.region
	}
}

#[derive(Debug, Clone)]
pub struct TileKeyMap<V, K: TileIndexKey = TileKey> {
	map: HashMap<K, V>,
	index: TileIndex<K>,
}

impl<V, K: TileIndexKey> Default for TileKeyMap<V, K> {
	fn default() -> Self {
		Self {
			map: HashMap::new(),
			index: TileIndex::default(),
		}
	}
}

impl<V, K: TileIndexKey> TileKeyMap<V, K> {
	pub fn insert(&mut self, tile_key: K, value: V) -> Option<V> {
		let out = self.map.insert(tile_key, value);
		if out.is_none() {
			self.index.insert(tile_key);
		}
		out
	}
	pub fn remove(&mut self, tile_key: K) -> Option<V> {
		let out = self.map.remove(&tile_key);
		if out.is_some() {
			self.index.remove(tile_key);
		}
		out
	}
	pub fn get(&self, tile_key: K) -> Option<&V> {
		self.map.get(&tile_key)
	}
	pub fn contains(&self, tile_key: K) -> bool {
		self.map.contains_key(&tile_key)
	}
	pub fn len(&self) -> usize {
		self.map.len()
	}
	pub fn is_empty(&self) -> bool {
		self.map.is_empty()
	}
	pub fn iter(&self) -> impl Iterator<Item = (K, &V)> {
		self.map.iter().map(|(&tile_key, value)| (tile_key, value))
	}
	pub fn for_each_overlapping(&self, region: NonZeroRegion, f: impl FnMut(K)) {
		self.index.for_each_overlapping(region, f);
	}
	pub fn keys_covering_point(&self, point: IVec3) -> Vec<K> {
		self.index.keys_covering_point(point)
	}
}

#[derive(Debug, Clone)]
pub struct TileKeySet<K: TileIndexKey = TileKey> {
	map: HashSet<K>,
	index: TileIndex<K>,
}

impl<K: TileIndexKey> Default for TileKeySet<K> {
	fn default() -> Self {
		Self {
			map: HashSet::new(),
			index: TileIndex::default(),
		}
	}
}

impl<K: TileIndexKey> TileKeySet<K> {
	pub fn insert(&mut self, tile_key: K) -> bool {
		let out = self.map.insert(tile_key);
		if out {
			self.index.insert(tile_key);
		}
		out
	}
	pub fn remove(&mut self, tile_key: K) -> bool {
		let out = self.map.remove(&tile_key);
		if out {
			self.index.remove(tile_key);
		}
		out
	}
	pub fn contains(&self, tile_key: K) -> bool {
		self.map.contains(&tile_key)
	}
	pub fn len(&self) -> usize {
		self.map.len()
	}
	pub fn is_empty(&self) -> bool {
		self.map.is_empty()
	}
	pub fn iter(&self) -> impl Iterator<Item = K> + '_ {
		self.map.iter().copied()
	}
	pub fn for_each_overlapping(&self, region: NonZeroRegion, f: impl FnMut(K)) {
		self.index.for_each_overlapping(region, f);
	}
	pub fn keys_covering_point(&self, point: IVec3) -> Vec<K> {
		self.index.keys_covering_point(point)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
	struct TestKey {
		id: u32,
		min: [i32; 3],
		size: [u32; 3],
	}

	impl TestKey {
		fn new(id: u32, min: IVec3, size: UVec3) -> Self {
			Self { id, min: min.to_array(), size: size.to_array() }
		}
	}

	impl TileIndexKey for TestKey {
		fn region(self) -> NonZeroRegion {
			NonZeroRegion::new(IVec3::from_array(self.min), UVec3::from_array(self.size)).unwrap()
		}
	}

	struct Rng(u64);

	impl Rng {
		fn next(&mut self) -> u64 {
			self.0 ^= self.0 << 13;
			self.0 ^= self.0 >> 7;
			self.0 ^= self.0 << 17;
			self.0
		}

		fn below(&mut self, limit: u32) -> u32 {
			(self.next() % limit as u64) as u32
		}

		fn position(&mut self, extent: i32) -> IVec3 {
			IVec3::new(self.below(extent as u32 * 2) as i32, self.below(extent as u32 * 2) as i32, self.below(extent as u32 * 2) as i32) - extent
		}

		/// A mix of aligned power-of-two cubes, as the region loader produces, and arbitrary boxes.
		fn region(&mut self, extent: i32) -> (IVec3, UVec3) {
			if self.below(2) == 0 {
				let size = 1u32 << self.below(11);
				(self.position(extent).div_euclid(IVec3::splat(size as i32)) * size as i32, UVec3::splat(size))
			} else {
				(self.position(extent), UVec3::new(self.below(700) + 1, self.below(700) + 1, self.below(700) + 1))
			}
		}
	}

	fn overlapping(index: &TileIndex<TestKey>, region: NonZeroRegion) -> Vec<TestKey> {
		let mut out = Vec::new();
		index.for_each_overlapping(region, |key| out.push(key));
		out.sort();
		out
	}

	fn assert_matches_brute_force(index: &TileIndex<TestKey>, keys: &[TestKey], rng: &mut Rng) {
		for _ in 0..300 {
			let (min, size) = rng.region(2500);
			let region = NonZeroRegion::new(min, size).unwrap();
			let mut expected: Vec<_> = keys.iter().copied().filter(|key| region.intersects(key.region())).collect();
			expected.sort();
			assert_eq!(overlapping(index, region), expected, "overlap query over {region:?}");

			let point = rng.position(2500);
			let mut expected: Vec<_> = keys.iter().copied().filter(|key| key.region().contains(point)).collect();
			expected.sort();
			let mut covering = index.keys_covering_point(point);
			covering.sort();
			assert_eq!(covering, expected, "point query at {point}");
		}
	}

	#[test]
	fn queries_match_brute_force_through_inserts_and_removes() {
		let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
		let mut index = TileIndex::default();
		let mut keys = Vec::new();
		for id in 0..600 {
			let (min, size) = rng.region(2000);
			let key = TestKey::new(id, min, size);
			index.insert(key);
			keys.push(key);
		}
		assert_matches_brute_force(&index, &keys, &mut rng);

		for key in keys.split_off(200) {
			index.remove(key);
		}
		assert_matches_brute_force(&index, &keys, &mut rng);

		for key in keys.drain(..) {
			index.remove(key);
		}
		assert!(index.is_empty());
		assert_eq!(index.keys_per_lod, [0; LOD_COUNT]);
	}

	#[test]
	fn repeated_inserts_and_removes_are_ignored() {
		let mut index = TileIndex::default();
		let key = TestKey::new(0, IVec3::new(-7, 3, 100), UVec3::new(40, 9, 9));
		let absent = TestKey::new(1, IVec3::new(-7, 3, 100), UVec3::new(40, 9, 9));
		index.insert(key);
		index.insert(key);
		index.remove(absent);
		assert_eq!(overlapping(&index, key.region()), [key]);
		index.remove(key);
		index.remove(key);
		assert!(index.is_empty());
	}

	#[test]
	fn huge_regions_only_visit_occupied_space() {
		let mut index = TileIndex::default();
		let near = TestKey::new(0, IVec3::splat(-128), UVec3::splat(64));
		let far = TestKey::new(1, IVec3::splat(1 << 29), UVec3::splat(4096));
		let huge = TestKey::new(2, IVec3::splat(-(1 << 30)), UVec3::splat(1 << 30));
		index.insert(near);
		index.insert(far);
		index.insert(huge);

		// Scanning every 64-unit bin of these regions would never finish.
		let world = NonZeroRegion::new(IVec3::splat(-(1 << 30)), UVec3::splat((1 << 31) - 1)).unwrap();
		assert_eq!(overlapping(&index, world), [near, far, huge]);
		let quadrant = NonZeroRegion::new(IVec3::splat(-(1 << 30)), UVec3::splat(1 << 30)).unwrap();
		assert_eq!(overlapping(&index, quadrant), [near, huge]);
		assert_eq!(index.keys_covering_point(IVec3::splat(-100)), [near, huge]);

		for key in [near, far, huge] {
			index.remove(key);
		}
		assert!(index.is_empty());
	}
}
