use std::{collections::{HashMap, HashSet}, hash::Hash};

use bevy::{app::MainScheduleOrder, ecs::{schedule::ScheduleLabel, system::SystemParam}, prelude::*};
use bevy_trait_query::One;

#[cfg(test)]
mod tests;

#[derive(Resource, Default)]
pub struct StateTick {
	current_tick: u64,
}

impl StateTick {
	fn last_tick(&self) -> u64 { self.current_tick.saturating_sub(1) }
}

pub trait DataType: 'static {
	type Params: Clone + Hash + PartialEq + Eq + Send + Sync + 'static;
	type Data: Clone + Send + Sync + 'static;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

#[bevy_trait_query::queryable]
pub trait DataProducingNode<DT: DataType> {
	fn cost(&self, request: &Request<DT>) -> Option<f32>; // None if the node can not fulfill the request
	fn request(&mut self, request: Request<DT>);
	fn drop(&mut self, params: &DT::Params); // also cancels a request that has not been passed yet
}

#[bevy_trait_query::queryable]
pub trait DataConsumingNode<DT: DataType> {
	fn pass_data(&mut self, params: &DT::Params, tick: u64, data: DT::Data);
	fn dirty_data(&mut self, params: &DT::Params, last_clean_tick: u64); // will be passed the diff later
}

pub struct DataEntry<DT: DataType> {
	pub dirty_state: DirtyState,
	pub data: DT::Data,
	pub source: Entity,
	usages: HashSet<Entity>,
}

#[derive(Resource)]
pub struct DataColumn<DT: DataType> {
	data: HashMap<DT::Params, DataEntry<DT>>,
	in_flight: HashMap<DT::Params, (Entity, DirtyState)>,
	data_to_drop: HashMap<DT::Params, HashSet<Entity>>, // this data is no longer requested from this Entity. Either no one wants it or something else provided it.
	maybe_unused: HashSet<DT::Params>,
	finished: HashMap<DT::Params, HashSet<Entity>>,
	waiting: HashMap<DT::Params, HashMap<Entity, DirtyState>>,
}

impl<DT: DataType> Default for DataColumn<DT> {
	fn default() -> Self {
		Self {
			data: HashMap::default(),
			in_flight: HashMap::default(),
			data_to_drop: HashMap::default(),
			maybe_unused: HashSet::default(),
			finished: HashMap::default(),
			waiting: HashMap::default(),
		}
	}
}

impl<DT: DataType> DataColumn<DT> {
	pub fn get(&self, params: &DT::Params) -> Option<&DataEntry<DT>> { self.data.get(params) }

	fn queue_drop(&mut self, params: &DT::Params, source: Entity) {
		self.data_to_drop.entry(params.clone()).or_default().insert(source);
	}

	fn unqueue_drop(&mut self, params: &DT::Params, source: Entity) {
		let Some(sources) = self.data_to_drop.get_mut(params) else { return; };
		sources.remove(&source);
		if sources.is_empty() {
			self.data_to_drop.remove(params);
		}
	}

	fn is_used(&self, params: &DT::Params) -> bool {
		self.waiting.contains_key(params)
			|| self.finished.contains_key(params)
			|| self.data.get(params).is_some_and(|entry| !entry.usages.is_empty())
	}
}

#[derive(SystemParam)]
pub struct DataRequester<'w, 's, DT: DataType> {
	data_column: ResMut<'w, DataColumn<DT>>,
	data_producing_node: Query<'w, 's, (One<&'static mut dyn DataProducingNode<DT>>, Entity)>,
}

impl<'w, 's, DT: DataType> DataRequester<'w, 's, DT> {
	pub fn request(&mut self, request: Request<DT>, requester_node: Entity) -> Option<DT::Data> {
		let column = &mut *self.data_column;
		if let Some(entry) = column.data.get_mut(&request.params) && entry.dirty_state.satisfies(&request.allowed_tick) {
			entry.usages.insert(requester_node);
			return Some(entry.data.clone());
		}
		column.waiting.entry(request.params.clone()).or_default().insert(requester_node, request.allowed_tick);
		if let Some((_, in_flight_tick)) = column.in_flight.get(&request.params) && in_flight_tick.satisfies(&request.allowed_tick) {
			return None;
		}

		let node_entity = Self::best_node(&self.data_producing_node, &request);
		if let Some((old_source, _)) = column.in_flight.insert(request.params.clone(), (node_entity, request.allowed_tick)) && old_source != node_entity {
			column.queue_drop(&request.params, old_source);
		}
		column.unqueue_drop(&request.params, node_entity);
		let mut node = self.data_producing_node.get_mut(node_entity).expect("This must exist as it was found in best_node").0;
		node.request(request);
		None
	}

	pub fn cancel(&mut self, params: &DT::Params, requester_node: Entity) {
		let column = &mut *self.data_column;
		if let Some(waiting) = column.waiting.get_mut(params) {
			waiting.remove(&requester_node);
			if waiting.is_empty() {
				column.waiting.remove(params);
			}
		}
		if let Some(finished) = column.finished.get_mut(params) {
			finished.remove(&requester_node);
			if finished.is_empty() {
				column.finished.remove(params);
			}
		}
		if let Some(entry) = column.data.get_mut(params) {
			entry.usages.remove(&requester_node);
		}
		column.maybe_unused.insert(params.clone());
	}

	fn best_node(data_producing_node: &Query<'w, 's, (One<&'static mut dyn DataProducingNode<DT>>, Entity)>, request: &Request<DT>) -> Entity {
		let mut best = (f32::MAX, None);
		for (node, entity) in data_producing_node.iter() {
			let Some(cost) = node.cost(request) else { continue; };
			if best.0 > cost {
				best.0 = cost;
				best.1 = Some(entity);
			}
		}
		let Some(entity) = best.1 else {
			panic!("Data request failed to find node to fulfill the request. This should never happen!");
		};
		entity
	}
}

#[derive(SystemParam)]
pub struct DataDirtier<'w, 's, DT: DataType> {
	bile_state_tick: Res<'w, StateTick>,
	data_column: ResMut<'w, DataColumn<DT>>,
	data_consuming_nodes: Query<'w, 's, One<&'static mut dyn DataConsumingNode<DT>>>,
}

impl<'w, 's, DT: DataType> DataDirtier<'w, 's, DT> {
	pub fn mark_dirty(&mut self, params: &DT::Params) {
		let Some(entry) = self.data_column.data.get_mut(params) else { return; };
		let last_clean_tick = self.bile_state_tick.last_tick();
		entry.dirty_state = DirtyState::Dirty { tick: last_clean_tick };
		entry.usages.retain(|&usage| {
			let Ok(mut data_consuming_node) = self.data_consuming_nodes.get_mut(usage) else {
				bevy::log::error!(name: "Missing DataConsumingNode", "Usage was not removed for non existent DataConsumingNode {}.", usage);
				return false;
			};
			data_consuming_node.dirty_data(params, last_clean_tick);
			true
		})
	}
}

#[derive(SystemParam)]
pub struct DataPasser<'w, DT: DataType> {
	data_column: ResMut<'w, DataColumn<DT>>,
}

impl<'w, DT: DataType> DataPasser<'w, DT> {
	pub fn pass_data(&mut self, source_entity: Entity, params: &DT::Params, dirty_state: DirtyState, data: DT::Data) {
		let column = &mut *self.data_column;
		let was_in_flight = column.in_flight.get(params).is_some_and(|(in_flight_source, required)| *in_flight_source == source_entity && dirty_state.satisfies(required));
		if was_in_flight {
			column.in_flight.remove(params);
		}
		if let Some(existing) = column.data.get(params) && existing.dirty_state.is_fresher_than(&dirty_state) {
			return;
		}

		let Some(waiting) = column.waiting.remove(params) else {
			if was_in_flight && column.data.get(params).is_none_or(|entry| entry.source != source_entity) {
				column.queue_drop(params, source_entity);
			}
			return; /* No need to store data thats not wanted */
		};

		match column.data.entry(params.clone()) {
			std::collections::hash_map::Entry::Occupied(mut entry) => {
				let entry = entry.get_mut();
				entry.dirty_state = dirty_state;
				entry.data = data;
				let old_source = std::mem::replace(&mut entry.source, source_entity);
				if old_source != source_entity {
					column.queue_drop(params, old_source);
				}
			}
			std::collections::hash_map::Entry::Vacant(entry) => {
				entry.insert(DataEntry { dirty_state, data, source: source_entity, usages: HashSet::default() });
			}
		}
		column.unqueue_drop(params, source_entity);

		let satisfied = column.finished.entry(params.clone()).or_default();
		let mut still_waiting = HashMap::default();
		for (entity, required) in waiting {
			if dirty_state.satisfies(&required) {
				satisfied.insert(entity);
			} else {
				still_waiting.insert(entity, required);
			}
		}
		if satisfied.is_empty() {
			column.finished.remove(params);
		}
		if !still_waiting.is_empty() {
			column.waiting.insert(params.clone(), still_waiting);
		}
	}
}

pub fn pass_data<DT: DataType>(
	bile_state_tick: Res<StateTick>,
	mut data_column: ResMut<DataColumn<DT>>,
	mut data_consuming_nodes: Query<One<&'static mut dyn DataConsumingNode<DT>>>,
) {
	if data_column.finished.is_empty() { return; }

	for (params, consumers) in std::mem::take(&mut data_column.finished) {
		let entry = data_column.data.get_mut(&params).expect("Its not new data if its not there!");
		let tick = match entry.dirty_state { DirtyState::Dirty { tick } => tick, DirtyState::UpToDate => bile_state_tick.current_tick };
		for entity in consumers {
			let Ok(mut data_consuming_node) = data_consuming_nodes.get_mut(entity) else { continue; };
			entry.usages.insert(entity);
			data_consuming_node.pass_data(&params, tick, entry.data.clone());
		}
	}
}

pub fn drop_data<DT: DataType>(
	mut data_column: ResMut<DataColumn<DT>>,
	mut data_producing_nodes: Query<One<&'static mut dyn DataProducingNode<DT>>>,
) {
	let column = &mut *data_column;
	for params in std::mem::take(&mut column.maybe_unused) {
		if column.is_used(&params) { continue; }
		if let Some(entry) = column.data.remove(&params) {
			column.queue_drop(&params, entry.source);
		}
		if let Some((source, _)) = column.in_flight.remove(&params) {
			column.queue_drop(&params, source);
		}
	}

	for (params, sources) in std::mem::take(&mut column.data_to_drop) {
		for source_entity in sources {
			let Ok(mut node) = data_producing_nodes.get_mut(source_entity) else { continue; };
			node.drop(&params);
		}
	}
}

fn update_bile_state_tick(mut bile_state_tick: ResMut<StateTick>) {
	bile_state_tick.current_tick += 1;
}

#[derive(ScheduleLabel, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BileDataSchedule;

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BileDataSystemSet {
	DropData,
	PassData,
	UpdateStateTick,
}

pub trait BileAddDataType {
	fn bile_add_data_type<DT: DataType>(&mut self) -> &mut Self;
}

impl BileAddDataType for App {
	fn bile_add_data_type<DT: DataType>(&mut self) -> &mut Self {
		self.add_systems(BileDataSchedule, (
			pass_data::<DT>.in_set(BileDataSystemSet::PassData),
			drop_data::<DT>.in_set(BileDataSystemSet::DropData),
		));
		self.init_resource::<DataColumn<DT>>()
	}
}

#[derive(Default)]
pub struct BileDataPlugin;

impl Plugin for BileDataPlugin {
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
		app.add_systems(BileDataSchedule, update_bile_state_tick.in_set(BileDataSystemSet::UpdateStateTick));
	}
}
