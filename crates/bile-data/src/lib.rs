use std::{collections::{HashMap, HashSet}, hash::Hash};

use bevy::{ecs::system::SystemParam, prelude::*};
use bevy_trait_query::One;

#[derive(Resource, Default)]
pub struct StateTick {
	current_tick: u64,
}

pub trait DataType: 'static {
	type Params: Clone + Hash + PartialEq + Eq + Send + Sync + 'static;
	type Data: Clone + Send + Sync + 'static;
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

#[bevy_trait_query::queryable]
pub trait DataProducingNode<DT: DataType> {
	fn cost(&self, request: &Request<DT>) -> Option<f32>; // None if the node can not fulfill the request
	fn request(&mut self, request: Request<DT>);
	fn drop(&mut self, params: &DT::Params);
}

#[bevy_trait_query::queryable]
pub trait DataConsumingNode<DT: DataType> {
	fn pass_data(&mut self, params: &DT::Params, tick: u64, data: DT::Data);
	fn dirty_data(&mut self, params: &DT::Params, last_clean_tick: u64); // will be passed the diff later
}

#[derive(Resource, Default)]
pub struct DataColumn<DT: DataType> {
	data: HashMap<DT::Params, (DirtyState, DT::Data, HashSet<Entity>, Entity)>,
	data_to_drop: HashMap<DT::Params, HashSet<Entity>>, // this data is no longer requested from this Entity. Either no one wants it or something else provided it.
	finished: HashMap<DT::Params, HashSet<Entity>>,
	waiting: HashMap<DT::Params, HashMap<Entity, DirtyState>>,
}

impl<DT: DataType> DataColumn<DT> {
	pub fn get(&self, params: &DT::Params) -> Option<&(DirtyState, DT::Data, HashSet<Entity>, Entity)> { self.data.get(params) }
}

#[derive(SystemParam)]
pub struct DataRequester<'w, 's, DT: DataType> {
	data_column: ResMut<'w, DataColumn<DT>>,
	data_producing_node: Query<'w, 's, (One<&'static mut dyn DataProducingNode<DT>>, Entity)>,
}

impl<'w, 's, DT: DataType> DataRequester<'w, 's, DT> {
	pub fn request(&mut self, request: Request<DT>, requester_node: Entity) -> Option<DT::Data> {
		if let Some((dirty_state, data, usages, _source_entity)) = self.data_column.data.get_mut(&request.params) {
			if dirty_state.satisfies(&request.allowed_tick) {
				usages.insert(requester_node);
				return Some(data.clone());
			}
		}
		// get node
		let node_entity = Self::best_node(&self.data_producing_node, &request);
		let mut node = self.data_producing_node.get_mut(node_entity).expect("This must exist as it was found in best_node").0;
		// update requester
		self.data_column.waiting.entry(request.params.clone()).or_default().insert(requester_node, request.allowed_tick);
		// create queue
		node.request(request);
		None
	}

	pub fn cancel(&mut self, params: &DT::Params, requester_node: Entity) {
		if let Some(waiting) = self.data_column.waiting.get_mut(params) {
			waiting.remove(&requester_node);
			if waiting.is_empty() {
				self.data_column.waiting.remove(params);
			}
		}
		let mut empty_finished = true;
		if let Some(finished) = self.data_column.finished.get_mut(params) {
			finished.remove(&requester_node);
			if finished.is_empty() {
				self.data_column.finished.remove(params);
			} else {
				empty_finished = false;
			}
		}

		let Some((_, _, usages, source_entity)) = self.data_column.data.get_mut(params) else { return; };
		usages.remove(&requester_node);

		if usages.is_empty() && empty_finished {
			let source_entity = *source_entity;
			if !self.data_column.waiting.contains_key(params) {
				self.data_column.data_to_drop.entry(params.clone()).or_default().insert(source_entity);
				self.data_column.data.remove(params);
			}
		}
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
	data_producing_nodes: Query<'w, 's, One<&'static mut dyn DataConsumingNode<DT>>>,
}

impl<'w, 's, DT: DataType> DataDirtier<'w, 's, DT> {
	pub fn mark_dirty(&mut self, params: &DT::Params) {
		let Some((dirty_state, _data, usages, _source_entity)) = &mut self.data_column.data.get_mut(params) else { return; };
		*dirty_state = DirtyState::Dirty { tick: self.bile_state_tick.current_tick - 1 /* Was valid on the last tick */ };
		usages.retain(|&usage| {
			let Ok(mut data_producing_node) = self.data_producing_nodes.get_mut(usage) else {
				bevy::log::error!(name: "Missing DataConsumingNode", "Usage was not removed for non existent DataConsumingNode {}.", usage);
				return false;
			};
			data_producing_node.dirty_data(params, self.bile_state_tick.current_tick - 1);
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
		if let Some((existing, _, _, _)) = self.data_column.data.get(params) && existing.is_fresher_than(&dirty_state) {
			return;
		}

		let Some(waiting) = self.data_column.waiting.remove(params) else { return; /* No need to store data thats not wanted */ };

		match self.data_column.data.entry(params.clone()) {
			std::collections::hash_map::Entry::Occupied(mut entry) => {
				let (old_dirty_state, old_data, _usages, old_source_entity) = entry.get_mut();
				*old_dirty_state = dirty_state;
				*old_data = data;
				if *old_source_entity != source_entity {
					let old_source_entity_copy = *old_source_entity;
					*old_source_entity = source_entity;
					let old_sources = self.data_column.data_to_drop.entry(params.clone()).or_default();
					old_sources.remove(&source_entity);
					old_sources.insert(old_source_entity_copy);
				}
			}
			std::collections::hash_map::Entry::Vacant(entry) => {
				entry.insert((dirty_state, data, HashSet::default(), source_entity));
				self.data_column.data_to_drop.entry(params.clone()).or_default().remove(&source_entity);
			}
		}

		let satisfied = self.data_column.finished.entry(params.clone()).or_default();
		let mut still_waiting = HashMap::default();
		for (entity, required) in waiting {
			if dirty_state.satisfies(&required) {
				satisfied.insert(entity);
			} else {
				still_waiting.insert(entity, required);
			}
		}
		if !still_waiting.is_empty() {
			self.data_column.waiting.insert(params.clone(), still_waiting);
		}
	}
}

pub fn pass_data<DT: DataType>(
	bile_state_tick: Res<StateTick>,
	mut data_column: ResMut<DataColumn<DT>>,
	mut data_consuming_nodes: Query<One<&'static mut dyn DataConsumingNode<DT>>>,
) {
	if data_column.finished.is_empty() { return; }

	for new_data_params in std::mem::replace(&mut data_column.finished, HashMap::default()) {
		let (dirty_state, new_data, usages, _source_entity) = &mut data_column.data.get_mut(&new_data_params.0).expect("Its not new data if its not there!");
		for entity in new_data_params.1 {
			let Ok(mut data_consuming_node) = data_consuming_nodes.get_mut(entity) else { continue; };
			usages.insert(entity);
			data_consuming_node.pass_data(
				&new_data_params.0,
				match dirty_state { DirtyState::Dirty { tick } => *tick, _ => bile_state_tick.current_tick },
				new_data.clone()
			);
		}
	}
}

pub fn drop_data<DT: DataType>(
	mut data_column: ResMut<DataColumn<DT>>,
	mut data_producing_nodes: Query<One<&'static mut dyn DataProducingNode<DT>>>,
) {
	if data_column.data_to_drop.is_empty() { return; }

	for (params, sources) in std::mem::replace(&mut data_column.data_to_drop, HashMap::default()) {
		for source_entity in sources {
			let Ok(mut node) = data_producing_nodes.get_mut(source_entity) else { continue; };
			node.drop(&params);
		}
	}
}
