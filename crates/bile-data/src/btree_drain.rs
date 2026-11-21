use std::collections::BTreeMap;
use std::ops::{Bound, RangeBounds};

pub fn drain_range<K: Ord, V>(map: &mut BTreeMap<K, V>, range: impl RangeBounds<K>) -> BTreeMap<K, V> {
	let mut tail = match range.start_bound() {
		Bound::Unbounded => std::mem::take(map),
		Bound::Included(k) => map.split_off(k),
		Bound::Excluded(k) => {
			let mut t = map.split_off(k);
			if let Some((k, v)) = t.remove_entry(k) {
				map.insert(k, v);
			}
			t
		}
	};

	let after = match range.end_bound() {
		Bound::Unbounded => BTreeMap::new(),
		Bound::Excluded(k) => tail.split_off(k),
		Bound::Included(k) => {
			let mut a = tail.split_off(k);
			if let Some((k, v)) = a.remove_entry(k) {
				tail.insert(k, v);
			}
			a
		}
	};

	map.extend(after);
	tail
}
