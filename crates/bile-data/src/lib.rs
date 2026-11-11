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

pub enum DirtyState {
	UpToDate,
	Dirty{tick: u64}
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
	fn drop(&mut self, request: &DT::Params);
}

#[bevy_trait_query::queryable]
pub trait DataConsumingNode<DT: DataType> {
	fn pass_data(&mut self, params: &DT::Params, tick: u64, data: DT::Data);
	fn dirty_data(&mut self, request: &DT::Params, tick: u64); // will be passed the diff later
}

#[derive(Resource, Default)]
pub struct DataColumn<DT: DataType> {
	data: HashMap<DT::Params, (DirtyState, DT::Data)>,
	new_data: HashSet<DT::Params>,
}

impl<DT: DataType> DataColumn<DT> {
	pub fn get(&self, params: &DT::Params) -> Option<&(DirtyState, DT::Data)> { self.data.get(params) }
}

#[derive(Resource, Default)]
pub struct RequestTracker<DT: DataType> {
	waiting: HashMap<DT::Params, HashSet<Entity>>,
}

#[derive(SystemParam)]
pub struct DataRequester<'w, 's, DT: DataType> {
	request_tracker: ResMut<'w, RequestTracker<DT>>,
	data_column: ResMut<'w, DataColumn<DT>>,
	data_producing_node: Query<'w, 's, (One<&'static mut dyn DataProducingNode<DT>>, Entity)>,
}

impl<'w, 's, DT: DataType> DataRequester<'w, 's, DT> {
	fn request(&mut self, request: Request<DT>, requester_node: Option<Entity>) -> Option<DT::Data> {
		if let Some((dirty_state, data)) = self.data_column.data.get(&request.params) {
			if let DirtyState::Dirty { tick } = *dirty_state {
				if let DirtyState::Dirty { tick: allowed_tick } = request.allowed_tick && tick >= allowed_tick {
					return Some(data.clone());
				}
			} else {
				return Some(data.clone());
			}
		}
		// get node
		let node_entity = Self::best_node(&self.data_producing_node, &request);
		let mut node = self.data_producing_node.get_mut(node_entity).expect("This must exist as it was found in best_node").0;
		// update requester
		if let Some(requester_node) = requester_node { self.request_tracker.waiting.entry(request.params.clone()).or_default().insert(requester_node); }
		// create queue
		node.request(request);
		None
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

pub fn pass_data<DT: DataType>(
	bile_state_tick: Res<StateTick>,
	request_tracker: ResMut<RequestTracker<DT>>,
	mut data_column: ResMut<DataColumn<DT>>,
	mut data_consuming_nodes: Query<One<&'static mut dyn DataConsumingNode<DT>>>,
) {
	if data_column.new_data.is_empty() { return; }

	for new_data_params in std::mem::replace(&mut data_column.new_data, HashSet::default()) {
		let (dirty_state, new_data) = data_column.data.get(&new_data_params).expect("Its not new data if its not there!");
		let Some(entities) = request_tracker.waiting.get(&new_data_params) else { continue; };
		for &entity in entities {
			let Ok(mut data_consuming_node) = data_consuming_nodes.get_mut(entity) else { continue; };
			data_consuming_node.pass_data(
				&new_data_params,
				match dirty_state { DirtyState::Dirty { tick } => *tick, _ => bile_state_tick.current_tick },
				new_data.clone()
			);
		}
	}
}
