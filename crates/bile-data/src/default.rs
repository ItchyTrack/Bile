use std::{hash::Hash, sync::atomic::{AtomicU64, Ordering}};

use bevy::{app::MainScheduleOrder, ecs::schedule::ScheduleLabel, prelude::*};

#[derive(Resource, Default)]
pub struct StateTick {
	current_tick: AtomicU64, // atomic so that everything that dirties can update it
}

impl StateTick {
	pub fn current_tick(&self) -> u64 {
		self.current_tick.load(Ordering::Relaxed)
	}

	// Returns the tick before the update
	pub(crate) fn update(&self) -> u64 {
		self.current_tick.fetch_add(1, Ordering::Relaxed)
	}
}

pub trait DataType: 'static {
	type Params: Clone + Hash + PartialEq + Eq + Send + Sync + 'static;
	type Data: Clone + Send + Sync + 'static;
	type Diff: Clone + Send + Sync + 'static;
}

#[derive(Clone, Copy)]
pub enum DirtyState {
	UpToDate,
	Dirty{tick: u64}
}

impl DirtyState {
	pub fn satisfies(&self, required: &DirtyState) -> bool {
		match (self, required) {
			(DirtyState::UpToDate, _) => true,
			(_, DirtyState::UpToDate) => false,
			(DirtyState::Dirty { tick: have }, DirtyState::Dirty { tick: need }) => *have >= *need,
		}
	}

	pub fn is_fresher_than(&self, other: &DirtyState) -> bool {
		match (self, other) {
			(DirtyState::UpToDate, DirtyState::UpToDate) => false,
			(DirtyState::UpToDate, _) => true,
			(_, DirtyState::UpToDate) => false,
			(DirtyState::Dirty { tick: a }, DirtyState::Dirty { tick: b }) => a > b,
		}
	}
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

#[derive(ScheduleLabel, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BileDataSchedule;

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BileDataSystemSet {
	DropData,
	PassData,
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
				BileDataSystemSet::DropData,
				BileDataSystemSet::PassData,
				BileDataSystemSet::UpdateStateTick,
			).chain(),
		);

		app.init_resource::<StateTick>();
	}
}
