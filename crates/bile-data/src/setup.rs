use std::{collections::BTreeSet, hash::Hash, ops::Bound};

use bevy::{app::MainScheduleOrder, ecs::schedule::ScheduleLabel, prelude::*};

use crate::btree_drain;

#[derive(Resource, Default)]
pub struct StateTick {
	current_tick: u64, // Update once per bile schedule run
}

impl StateTick {
	pub fn current_tick(&self) -> u64 {
		self.current_tick
	}
}

fn update_state_tick(mut state_tick: ResMut<StateTick>) {
	state_tick.current_tick += 1;
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum DirtyState {
	UpToDate,
	Dirty{tick: u64}
}

impl DirtyState {
	pub fn satisfies(self, required: DirtyState) -> bool {
		match (self, required) {
			(DirtyState::UpToDate, _) => true,
			(_, DirtyState::UpToDate) => false,
			(DirtyState::Dirty { tick: have }, DirtyState::Dirty { tick: need }) => have >= need,
		}
	}

	pub fn is_fresher_than(self, other: DirtyState) -> bool {
		match (self, other) {
			(DirtyState::UpToDate, DirtyState::UpToDate) => false,
			(DirtyState::UpToDate, _) => true,
			(_, DirtyState::UpToDate) => false,
			(DirtyState::Dirty { tick: a }, DirtyState::Dirty { tick: b }) => a > b,
		}
	}

	// If a cancel of oldest..=latest covers it
	pub fn is_cancelled_by(self, latest: DirtyState, oldest: Option<u64>) -> bool {
		self <= latest && !matches!((self, oldest), (DirtyState::Dirty { tick }, Some(oldest)) if tick < oldest)
	}
}

pub fn cancel_range(latest: DirtyState, oldest: Option<u64>) -> (Bound<DirtyState>, Bound<DirtyState>) {
	(oldest.map_or(Bound::Unbounded, |oldest| Bound::Included(DirtyState::Dirty { tick: oldest })), Bound::Included(latest))
}

// The parts of a cancel that cover none of the held states, as (latest, oldest). These can be cancelled further on.
pub fn unheld_ranges(held: impl IntoIterator<Item = DirtyState>, latest: DirtyState, oldest: Option<u64>) -> Vec<(DirtyState, Option<u64>)> {
	// UpToDate is the tick after the last tick
	let position = |state: DirtyState| match state { DirtyState::Dirty { tick } => tick as u128, DirtyState::UpToDate => u64::MAX as u128 + 1 };
	let state = |position: u128| if position > u64::MAX as u128 { DirtyState::UpToDate } else { DirtyState::Dirty { tick: position as u64 } };
	let mut held: Vec<u128> = held.into_iter().filter(|held| held.is_cancelled_by(latest, oldest)).map(position).collect();
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

// What a client holds at its server for some params: the requests it still waits for, and the wanted state of its
// latest satisfied request. Older satisfied requests are thrown away.
#[derive(Default)]
pub struct Claim {
	pub waiting: BTreeSet<DirtyState>,
	pub usage: Option<DirtyState>,
}

impl Claim {
	pub fn is_empty(&self) -> bool { self.waiting.is_empty() && self.usage.is_none() }
	pub fn held(&self) -> impl Iterator<Item = DirtyState> + '_ { self.waiting.iter().copied().chain(self.usage) }
	pub fn request(&mut self, wanted: DirtyState, served: bool) {
		if served { self.usage = Some(wanted); } else { self.waiting.insert(wanted); }
	}
	// Returns true if the data satisfied a wait, the freshest of them is the usage then
	pub fn pass(&mut self, dirty_state: DirtyState) -> bool {
		let satisfied = btree_drain::set_drain_range(&mut self.waiting, ..=dirty_state);
		let Some(&wanted) = satisfied.last() else { return false; };
		self.usage = Some(wanted);
		true
	}
	// Returns true if it held something the cancel covers
	pub fn cancel(&mut self, latest: DirtyState, oldest: Option<u64>) -> bool {
		let waited = !btree_drain::set_drain_range(&mut self.waiting, cancel_range(latest, oldest)).is_empty();
		let used = self.usage.is_some_and(|usage| usage.is_cancelled_by(latest, oldest));
		if used { self.usage = None; }
		waited || used
	}
}

impl PartialOrd for DirtyState {
	fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for DirtyState {
	fn cmp(&self, other: &Self) -> std::cmp::Ordering {
		match (self, other) {
			(DirtyState::UpToDate, DirtyState::UpToDate) => std::cmp::Ordering::Equal,
			(DirtyState::UpToDate, _) => std::cmp::Ordering::Greater,
			(_, DirtyState::UpToDate) => std::cmp::Ordering::Less,
			(DirtyState::Dirty { tick: a }, DirtyState::Dirty { tick: b }) => a.cmp(b),
		}
	}
}

pub trait DataType: 'static {
	type Params: Clone + Hash + PartialEq + Eq + Send + Sync + 'static;
	type Data: Clone + Send + Sync + 'static;
	type Diff: Clone + Send + Sync + 'static;
}

pub struct Request<DT: DataType> {
	pub params: DT::Params,
	pub allowed_tick: DirtyState, // UpToDate -> Only up to date data; Dirty(tick) -> Reject data before tick
}

impl<DT: DataType> Request<DT> {
	pub fn new(params: DT::Params, allowed_tick: DirtyState) -> Self {
		Self { params, allowed_tick }
	}
}

impl<DT: DataType> Clone for Request<DT> {
	fn clone(&self) -> Self {
		Self { params: self.params.clone(), allowed_tick: self.allowed_tick }
	}
}

#[derive(ScheduleLabel, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BileDataSchedule;

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BileDataSystemSet {
	PropagateDirty,
	Cancel,
	Request,
	PassData,
	Store, // Storages move the data to where it is kept
	UpdateStateTick,
}

pub struct BileDataDefaultPlugin;

impl Plugin for BileDataDefaultPlugin {
	fn build(&self, app: &mut App) {
		app.init_schedule(BileDataSchedule);
		app.world_mut()
			.resource_mut::<MainScheduleOrder>()
			.insert_after(Update, BileDataSchedule);

		app.configure_sets(
			BileDataSchedule,
			(
				BileDataSystemSet::PropagateDirty,
				BileDataSystemSet::Cancel,
				BileDataSystemSet::Request,
				BileDataSystemSet::PassData,
				BileDataSystemSet::Store,
				BileDataSystemSet::UpdateStateTick,
			).chain(),
		);

		app.init_resource::<StateTick>();
		app.add_systems(BileDataSchedule, update_state_tick.in_set(BileDataSystemSet::UpdateStateTick));
	}
}
