use std::ops::Bound;

use crate::DirtyState;

// If a cancel of oldest..=latest covers the dirty state
pub fn is_cancelled(dirty_state: DirtyState, latest: DirtyState, oldest: Option<u64>) -> bool {
	dirty_state <= latest && !matches!((dirty_state, oldest), (DirtyState::Dirty { tick }, Some(oldest)) if tick < oldest)
}

pub fn cancel_range(latest: DirtyState, oldest: Option<u64>) -> (Bound<DirtyState>, Bound<DirtyState>) {
	(oldest.map_or(Bound::Unbounded, |oldest| Bound::Included(DirtyState::Dirty { tick: oldest })), Bound::Included(latest))
}

// The parts of a cancel that cover none of the held states, as (latest, oldest). These can be cancelled further on.
pub fn unheld_ranges(held: impl IntoIterator<Item = DirtyState>, latest: DirtyState, oldest: Option<u64>) -> Vec<(DirtyState, Option<u64>)> {
	// UpToDate is the tick after the last tick
	let position = |state: DirtyState| match state { DirtyState::Dirty { tick } => tick as u128, DirtyState::UpToDate => u64::MAX as u128 + 1 };
	let state = |position: u128| if position > u64::MAX as u128 { DirtyState::UpToDate } else { DirtyState::Dirty { tick: position as u64 } };
	let mut held: Vec<u128> = held.into_iter().filter(|&held| is_cancelled(held, latest, oldest)).map(position).collect();
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
