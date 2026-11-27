use std::ops::{Bound, RangeBounds};

use crate::DirtyState;

pub fn in_cancel_range(dirty_state: DirtyState, latest: DirtyState, oldest: Option<u64>) -> bool {
	cancel_range(latest, oldest).contains(&dirty_state)
}

pub fn cancel_range(latest: DirtyState, oldest: Option<u64>) -> (Bound<DirtyState>, Bound<DirtyState>) {
	(oldest.map_or(Bound::Unbounded, |oldest| Bound::Included(DirtyState::Dirty { tick: oldest })), Bound::Included(latest))
}

// Removes held from [latest, oldest] returning a vector of bounds
pub fn unheld_ranges(held: impl IntoIterator<Item = DirtyState>, latest: DirtyState, oldest: Option<u64>) -> Vec<(DirtyState, Option<u64>)> {
	// UpToDate is the tick after the last tick
	let position = |state: DirtyState| match state { DirtyState::Dirty { tick } => tick as u128, DirtyState::UpToDate => u64::MAX as u128 + 1 };
	let state = |position: u128| if position > u64::MAX as u128 { DirtyState::UpToDate } else { DirtyState::Dirty { tick: position as u64 } };
	let mut held: Vec<u128> = held.into_iter().filter(|&held| in_cancel_range(held, latest, oldest)).map(position).collect();
	held.sort_unstable();
	held.dedup();
	let mut ranges = Vec::new();
	let mut start = oldest.unwrap_or(0) as u128;
	for end in held.into_iter().chain([position(latest) + 1]) {
		if start < end {
			ranges.push((state(end - 1), (start > 0).then(|| start.min(u64::MAX as u128) as u64)));
		}
		start = end + 1;
	}
	ranges
}
