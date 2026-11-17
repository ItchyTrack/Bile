use std::collections::{HashMap, HashSet};

use bevy::{
	ecs::system::{DynParamBuilder, DynSystemParam, ParamBuilder, SystemParam},
	prelude::*,
};
use bevy_trait_query::{One, RegisterExt};

use crate::{default::{BileDataSchedule, BileDataSystemSet, DataType, DirtyState, Request, StateTick}, diff::{self, DiffApplyingNodes}, dyn_system_params::{DynSystemParams, HasDynSystemParam, downcast}};

// Simple Consuming

#[bevy_trait_query::queryable]
pub trait DataConsumingNode<DT: DataType> {
	fn pass_data(&mut self, params: &DT::Params, tick: u64, data: DT::Data);
	fn dirty_data(&mut self, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>);
}

#[bevy_trait_query::queryable]
pub trait DataRequestingAPI<DT: DataType> {
	fn dyn_system_param(&self) -> DynParamBuilder<'static>;
	fn cost(&self, dyn_system_params: &mut [DynSystemParam], request: &Request<DT>) -> Option<f32>;
	fn request(&mut self, dyn_system_params: &mut [DynSystemParam], request: Request<DT>, requester_node: Entity) -> Option<DT::Data>;
	fn cancel(&mut self, dyn_system_params: &mut [DynSystemParam], params: &DT::Params, requester_node: Entity);
}

#[derive(SystemParam)]
pub struct DataRequester<'w, 's, DT: DataType> {
	data_requesting_apis: Query<'w, 's, (One<&'static mut dyn DataRequestingAPI<DT>>, Entity)>,
	dyn_system_params: DynSystemParams<'w, 's, dyn DataRequestingAPI<DT>>,
}

impl<DT: DataType> HasDynSystemParam for dyn DataRequestingAPI<DT> {
	fn dyn_system_param(&self) -> DynParamBuilder<'static> { DataRequestingAPI::dyn_system_param(self) }
}

impl<'w, 's, DT: DataType> DataRequester<'w, 's, DT> {
	pub fn request(&mut self, request: Request<DT>, requester_node: Entity) -> Option<DT::Data> {
		let api_entity = Self::best_api(&self.data_requesting_apis, &mut self.dyn_system_params.0, &request);
		for (mut data_requesting_api, entity) in self.data_requesting_apis.iter_mut() {
			if entity == api_entity { continue; }
			data_requesting_api.cancel(&mut self.dyn_system_params.0, &request.params, requester_node);
		}
		self.data_requesting_apis.get_mut(api_entity).expect("This must exist as it was found in best_node").0.request(&mut self.dyn_system_params.0, request, requester_node)
	}
	pub fn cancel(&mut self, params: &DT::Params, requester_node: Entity) {
		for (mut data_requesting_api, _entity) in self.data_requesting_apis.iter_mut() {
			data_requesting_api.cancel(&mut self.dyn_system_params.0, params, requester_node);
		}
	}
	fn best_api(data_requesting_apis: &Query<'w, 's, (One<&'static mut dyn DataRequestingAPI<DT>>, Entity)>, dyn_system_params: &mut [DynSystemParam], request: &Request<DT>) -> Entity {
		let mut best = (f32::MAX, None);
		for (data_requesting_api, entity) in data_requesting_apis.iter() {
			let Some(cost) = data_requesting_api.cost(dyn_system_params, request) else { continue; };
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

// Simple Producing

#[bevy_trait_query::queryable]
pub trait DataProducingNode<DT: DataType> {
	fn cost(&self, request: &Request<DT>) -> Option<f32>; // None if the node can not fulfill the request
	fn request(&mut self, request: Request<DT>);
	fn drop(&mut self, params: &DT::Params);
}

pub type DataProducingNodes<'w, 's, DT> = Query<'w, 's, (One<&'static mut dyn DataProducingNode<DT>>, Entity)>;
pub type DataConsumingNodes<'w, 's, DT> = Query<'w, 's, One<&'static mut dyn DataConsumingNode<DT>>>;

// None if no node can fulfill the request
pub fn best_node<DT: DataType>(data_producing_node: &DataProducingNodes<DT>, request: &Request<DT>) -> Option<(f32, Entity)> {
	let mut best = (f32::MAX, None);
	for (node, entity) in data_producing_node.iter() {
		let Some(cost) = node.cost(request) else { continue; };
		if best.0 > cost {
			best.0 = cost;
			best.1 = Some(entity);
		}
	}
	Some((best.0, best.1?))
}

// Every storage gets what the nodes pass and dirty, there can be any number of them and they do not know each other.
#[bevy_trait_query::queryable]
pub trait DataStorage<DT: DataType> {
	fn pass_data(&mut self, source_entity: Entity, params: &DT::Params, dirty_state: DirtyState, data: &DT::Data);
	fn dirty_data(&mut self, data_consuming_nodes: &mut DataConsumingNodes<DT>, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>);
	fn uses(&self, params: &DT::Params, source_entity: Entity) -> bool; // a node is only dropped when no storage uses it
	fn drop_data(&mut self) -> HashMap<DT::Params, HashSet<Entity>>; // the nodes this storage no longer uses
}

#[derive(SystemParam)]
pub struct DataDirtier<'w, 's, DT: DataType> {
	bile_state_tick: Res<'w, StateTick>,
	data_storages: Query<'w, 's, One<&'static mut dyn DataStorage<DT>>>,
	data_consuming_nodes: DataConsumingNodes<'w, 's, DT>,
}

impl<'w, 's, DT: DataType> DataDirtier<'w, 's, DT> {
	pub fn mark_dirty(&mut self, params: &DT::Params, diff: Option<&DT::Diff>) {
		for mut data_storage in &mut self.data_storages {
			data_storage.dirty_data(&mut self.data_consuming_nodes, params, self.bile_state_tick.current_tick() - 1 /* Was valid on the last tick */, diff);
		}
	}
}

#[derive(SystemParam)]
pub struct DataPasser<'w, 's, DT: DataType> {
	data_storages: Query<'w, 's, One<&'static mut dyn DataStorage<DT>>>,
}

impl<'w, 's, DT: DataType> DataPasser<'w, 's, DT> {
	pub fn pass_data(&mut self, source_entity: Entity, params: &DT::Params, dirty_state: DirtyState, data: DT::Data) {
		for mut data_storage in &mut self.data_storages {
			data_storage.pass_data(source_entity, params, dirty_state, &data);
		}
	}
}

pub fn drop_data<DT: DataType>(
	mut data_storages: Query<One<&'static mut dyn DataStorage<DT>>>,
	mut data_producing_nodes: Query<One<&'static mut dyn DataProducingNode<DT>>>,
) {
	let mut data_to_drop: HashMap<DT::Params, HashSet<Entity>> = HashMap::default();
	for mut data_storage in &mut data_storages {
		for (params, sources) in data_storage.drop_data() {
			data_to_drop.entry(params).or_default().extend(sources);
		}
	}
	for (params, sources) in data_to_drop {
		for source_entity in sources {
			if data_storages.iter().any(|data_storage| data_storage.uses(&params, source_entity)) { continue; }
			let Ok(mut node) = data_producing_nodes.get_mut(source_entity) else { continue; };
			node.drop(&params);
		}
	}
}

// Simple Producing Data
pub struct DataInfo<DT: DataType> {
	pub dirty_state: DirtyState,
	pub data: Option<DT::Data>,
	pub usages: HashSet<Entity>,
	pub source_entity: Entity,
	chain: Vec<Entity>, // earlier sources, whose out of date data is not stored
	diffs: Option<Vec<DT::Diff>>, // every dirty since the data was made. None if one had no diff
}

#[derive(Resource)]
pub struct DataColumn<DT: DataType> {
	data: HashMap<DT::Params, DataInfo<DT>>,
	data_to_drop: HashMap<DT::Params, HashSet<Entity>>, // this data is no longer requested from this Entity. Either no one wants it or something else provided it.
	finished: HashMap<DT::Params, HashSet<Entity>>,
	waiting: HashMap<DT::Params, HashMap<Entity, DirtyState>>,
}

impl<DT: DataType> Default for DataColumn<DT> {
	fn default() -> Self {
		Self {
			data: HashMap::default(),
			data_to_drop: HashMap::default(),
			finished: HashMap::default(),
			waiting: HashMap::default(),
		}
	}
}

type DataNodes<'w, 's, DT> = ParamSet<'w, 's, (DataProducingNodes<'static, 'static, DT>, DiffApplyingNodes<'static, 'static, DT>)>;

impl<DT: DataType> DataColumn<DT> {
	pub fn get(&self, params: &DT::Params) -> Option<&DataInfo<DT>> { self.data.get(params) }
	fn best_diff(&self, diff_applying_node: &DiffApplyingNodes<DT>, request: &Request<DT>) -> Option<(f32, Entity)> {
		let info = self.data.get(&request.params).filter(|info| info.data.is_some() && !info.dirty_state.satisfies(&request.allowed_tick))?;
		diff::best_node(diff_applying_node, request, info.diffs.as_ref()?)
	}
}

impl<DT: DataType> DataRequestingAPI<DT> for DataColumn<DT> {
	fn dyn_system_param(&self) -> DynParamBuilder<'static> { DynParamBuilder::new::<DataNodes<DT>>(ParamBuilder) }
	fn cost(&self, dyn_system_params: &mut [DynSystemParam], request: &Request<DT>) -> Option<f32> {
		let mut data_nodes: DataNodes<DT> = downcast(dyn_system_params);
		let diff_cost = self.best_diff(&data_nodes.p1(), request).map(|best| best.0);
		let node_cost = best_node(&data_nodes.p0(), request).map(|best| best.0);
		[diff_cost, node_cost].into_iter().flatten().reduce(f32::min)
	}
	fn request(&mut self, dyn_system_params: &mut [DynSystemParam], request: Request<DT>, requester_node: Entity) -> Option<DT::Data> {
		let mut data_nodes: DataNodes<DT> = downcast(dyn_system_params);
		let best_node = best_node(&data_nodes.p0(), &request);
		if let Some((cost, node_entity)) = self.best_diff(&data_nodes.p1(), &request) && best_node.is_none_or(|best_node| cost < best_node.0) {
			let info = self.data.get_mut(&request.params).expect("This must exist as it was found in best_diff");
			let (data, diffs) = (info.data.as_mut().expect("This must exist as it was found in best_diff"), info.diffs.replace(Vec::new()).expect("This must exist as it was found in best_diff"));
			let applied = data_nodes.p1().get_mut(node_entity).expect("This must exist as it was found in best_diff").0.apply(&request.params, data, diffs);
			info.dirty_state = DirtyState::UpToDate;
			if info.source_entity != node_entity {
				info.chain.push(std::mem::replace(&mut info.source_entity, node_entity));
			}
			if !applied {
				info.data = None; // out of date and not worth caching
				self.waiting.entry(request.params).or_default().insert(requester_node, request.allowed_tick);
				return None;
			}
		}
		if let Some(info) = self.data.get_mut(&request.params) {
			if info.dirty_state.satisfies(&request.allowed_tick) {
				if let Some(data) = &info.data {
					info.usages.insert(requester_node);
					return Some(data.clone());
				}
			}
		}
		let already_requested = self.waiting.get(&request.params).is_some_and(|waiting| waiting.values().any(|required| required.satisfies(&request.allowed_tick)));
		// update requester
		self.waiting.entry(request.params.clone()).or_default().insert(requester_node, request.allowed_tick);
		if already_requested { return None; }
		// get node
		let node_entity = best_node.expect("Data request failed to find node to fulfill the request. This should never happen!").1;
		if let Some(sources) = self.data_to_drop.get_mut(&request.params) {
			sources.remove(&node_entity);
		}
		// create queue
		data_nodes.p0().get_mut(node_entity).expect("This must exist as it was found in best_node").0.request(request);
		None
	}
	fn cancel(&mut self, _dyn_system_params: &mut [DynSystemParam], params: &DT::Params, requester_node: Entity) {
		if let Some(waiting) = self.waiting.get_mut(params) {
			waiting.remove(&requester_node);
			if waiting.is_empty() {
				self.waiting.remove(params);
			}
		}
		let mut empty_finished = true;
		if let Some(finished) = self.finished.get_mut(params) {
			finished.remove(&requester_node);
			if finished.is_empty() {
				self.finished.remove(params);
			} else {
				empty_finished = false;
			}
		}

		let Some(info) = self.data.get_mut(params) else { return; };
		info.usages.remove(&requester_node);
		if info.usages.is_empty() && empty_finished && !self.waiting.contains_key(params) {
			let sources = self.data_to_drop.entry(params.clone()).or_default();
			sources.insert(info.source_entity);
			sources.extend(info.chain.drain(..));
			self.data.remove(params);
		}
	}
}

impl<DT: DataType> DataStorage<DT> for DataColumn<DT> {
	fn pass_data(&mut self, source_entity: Entity, params: &DT::Params, dirty_state: DirtyState, data: &DT::Data) {
		if let Some(existing) = self.data.get(params) && existing.dirty_state.is_fresher_than(&dirty_state) {
			return;
		}

		let Some(waiting) = self.waiting.remove(params) else {
			if self.data.get(params).is_none_or(|existing| existing.source_entity != source_entity) {
				self.data_to_drop.entry(params.clone()).or_default().insert(source_entity);
			}
			return; /* No need to store data thats not wanted */
		};

		let diffs = matches!(dirty_state, DirtyState::UpToDate).then(Vec::new);
		let dirty_state = match self.data.entry(params.clone()) {
			std::collections::hash_map::Entry::Occupied(mut entry) => {
				let info = entry.get_mut();
				info.data = Some(data.clone());
				if info.source_entity != source_entity {
					let old_sources = self.data_to_drop.entry(params.clone()).or_default();
					old_sources.extend(info.chain.drain(..));
					old_sources.insert(std::mem::replace(&mut info.source_entity, source_entity));
					old_sources.remove(&source_entity);
				}
				// The data of a diff applier lacks what was dirtied after it got its diffs.
				if info.chain.is_empty() || info.diffs.as_ref().is_some_and(Vec::is_empty) {
					info.dirty_state = dirty_state;
					info.diffs = diffs;
				}
				info.dirty_state
			}
			std::collections::hash_map::Entry::Vacant(entry) => {
				entry.insert(DataInfo { dirty_state, data: Some(data.clone()), usages: HashSet::default(), source_entity, chain: Vec::new(), diffs });
				self.data_to_drop.entry(params.clone()).or_default().remove(&source_entity);
				dirty_state
			}
		};

		let satisfied = self.finished.entry(params.clone()).or_default();
		let mut still_waiting = HashMap::default();
		for (entity, required) in waiting {
			if dirty_state.satisfies(&required) {
				satisfied.insert(entity);
			} else {
				still_waiting.insert(entity, required);
			}
		}
		if !still_waiting.is_empty() {
			self.waiting.insert(params.clone(), still_waiting);
		}
	}
	fn dirty_data(&mut self, data_consuming_nodes: &mut DataConsumingNodes<DT>, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>) {
		let Some(info) = self.data.get_mut(params) else { return; };
		info.dirty_state = DirtyState::Dirty { tick: last_clean_tick };
		match (&mut info.diffs, diff) {
			(Some(diffs), Some(diff)) => diffs.push(diff.clone()),
			_ => info.diffs = None, // There may never be a dirty inbetween 2 diffs
		}
		info.usages.retain(|&usage| {
			let Ok(mut data_consuming_node) = data_consuming_nodes.get_mut(usage) else {
				bevy::log::error!(name: "Missing DataConsumingNode", "Usage was not removed for non existent DataConsumingNode {}.", usage);
				return false;
			};
			data_consuming_node.dirty_data(params, last_clean_tick, diff);
			true
		})
	}
	fn uses(&self, params: &DT::Params, source_entity: Entity) -> bool {
		self.waiting.contains_key(params) || self.data.get(params).is_some_and(|info| info.source_entity == source_entity || info.chain.contains(&source_entity))
	}
	fn drop_data(&mut self) -> HashMap<DT::Params, HashSet<Entity>> { std::mem::take(&mut self.data_to_drop) }
}

pub fn pass_data<DT: DataType>(
	bile_state_tick: Res<StateTick>,
	mut data_column: ResMut<DataColumn<DT>>,
	mut data_consuming_nodes: DataConsumingNodes<DT>,
) {
	if data_column.finished.is_empty() { return; }

	for (params, entities) in std::mem::take(&mut data_column.finished) {
		let info = data_column.data.get_mut(&params).expect("Its not new data if its not there!");
		for entity in entities {
			let Some(data) = info.data.clone() else { continue; };
			let Ok(mut data_consuming_node) = data_consuming_nodes.get_mut(entity) else { continue; };
			info.usages.insert(entity);
			data_consuming_node.pass_data(
				&params,
				match info.dirty_state { DirtyState::Dirty { tick } => tick, _ => bile_state_tick.current_tick() },
				data
			);
		}
	}
}

fn setup_bevy_resources<DT: DataType>(app: &mut App) {
	if app.world().contains_resource::<DataColumn<DT>>() { return; }
	app
		.add_systems(BileDataSchedule, (
			pass_data::<DT>.in_set(BileDataSystemSet::PassData),
			drop_data::<DT>.in_set(BileDataSystemSet::DropData),
		))
		.init_resource::<DataColumn<DT>>()
		.register_component_as::<dyn DataRequestingAPI<DT>, DataColumn<DT>>()
		.register_component_as::<dyn DataStorage<DT>, DataColumn<DT>>();
}

pub trait BileRegisterSimple {
	fn register_producer_simple<DT, C>(&mut self) -> &mut Self
    where DT: DataType, C: Component + DataProducingNode<DT>;
	fn register_consumer_simple<DT, C>(&mut self) -> &mut Self
    where DT: DataType, C: Component + DataConsumingNode<DT>;
}

impl BileRegisterSimple for App {
    fn register_producer_simple<DT, C>(&mut self) -> &mut Self
    where DT: DataType, C: Component + DataProducingNode<DT>,
    {
        self.register_component_as::<dyn DataProducingNode<DT>, C>();
		setup_bevy_resources::<DT>(self);
        self
    }

    fn register_consumer_simple<DT, C>(&mut self) -> &mut Self
    where DT: DataType, C: Component + DataConsumingNode<DT>,
    {
        self.register_component_as::<dyn DataConsumingNode<DT>, C>();
		setup_bevy_resources::<DT>(self);
        self
    }
}
