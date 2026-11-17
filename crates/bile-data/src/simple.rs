use std::collections::{HashMap, HashSet};

use bevy::{
	ecs::{
		change_detection::Tick,
		system::{DynParamBuilder, DynSystemParam, DynSystemParamState, ParamBuilder, SystemAccess, SystemMeta, SystemParam, SystemParamValidationError},
		world::{DeferredWorld, unsafe_world_cell::UnsafeWorldCell},
	},
	prelude::*,
};
use bevy_trait_query::{One, RegisterExt};

use crate::default::{BileDataSchedule, BileDataSystemSet, DataType, DirtyState, Request, StateTick};

// Simple Consuming

#[bevy_trait_query::queryable]
pub trait DataConsumingNode<DT: DataType> {
	fn pass_data(&mut self, params: &DT::Params, tick: u64, data: DT::Data);
	fn dirty_data(&mut self, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>); // will be passed the diff later
}

#[bevy_trait_query::queryable]
pub trait DataRequestingAPI<DT: DataType> {
	fn dyn_system_param(&self) -> DynParamBuilder<'static>;
	fn cost(&self, dyn_system_params: &mut [DynSystemParam], request: &Request<DT>) -> Option<f32>;
	fn request(&mut self, dyn_system_params: &mut [DynSystemParam], request: Request<DT>, requester_node: Entity, cache: bool) -> Option<DT::Data>;
	fn cancel(&mut self, dyn_system_params: &mut [DynSystemParam], params: &DT::Params, requester_node: Entity);
}

pub struct DataRequester<'w, 's, DT: DataType> {
	data_requesting_apis: Query<'w, 's, (One<&'static mut dyn DataRequestingAPI<DT>>, Entity)>,
	dyn_system_params: Vec<DynSystemParam<'w, 's>>,
}

// SAFETY: Delegates to the tuple of its fields, only the states of the `DynSystemParam`s are built by the APIs instead of being empty.
unsafe impl<DT: DataType> SystemParam for DataRequester<'_, '_, DT> {
	type State = (QueryState<(One<&'static mut dyn DataRequestingAPI<DT>>, Entity)>, Vec<DynSystemParamState>);
	type Item<'w, 's> = DataRequester<'w, 's, DT>;
	fn init_state(world: &mut World) -> Self::State {
		let dyn_system_params: Vec<_> = world.query::<One<&dyn DataRequestingAPI<DT>>>().iter(world).map(|data_requesting_api| data_requesting_api.dyn_system_param()).collect();
		(Query::init_state(world), dyn_system_params.into_iter().map(|dyn_system_param| dyn_system_param.build(world)).collect())
	}
	fn init_access(state: &Self::State, system_meta: &mut SystemMeta, system_access: &mut SystemAccess, world: &mut World) {
		<(Query<_>, Vec<DynSystemParam>)>::init_access(state, system_meta, system_access, world);
	}
	fn apply(state: &mut Self::State, system_meta: &SystemMeta, world: &mut World) { <(Query<_>, Vec<DynSystemParam>)>::apply(state, system_meta, world); }
	fn queue(state: &mut Self::State, system_meta: &SystemMeta, world: DeferredWorld) { <(Query<_>, Vec<DynSystemParam>)>::queue(state, system_meta, world); }
	unsafe fn get_param<'w, 's>(state: &'s mut Self::State, system_meta: &SystemMeta, world: UnsafeWorldCell<'w>, change_tick: Tick) -> Result<Self::Item<'w, 's>, SystemParamValidationError> {
		let (data_requesting_apis, dyn_system_params) = unsafe { <(Query<_>, Vec<DynSystemParam>)>::get_param(state, system_meta, world, change_tick) }?;
		Ok(DataRequester { data_requesting_apis, dyn_system_params })
	}
}

impl<'w, 's, DT: DataType> DataRequester<'w, 's, DT> {
	pub fn request(&mut self, request: Request<DT>, requester_node: Entity, cache: bool) -> Option<DT::Data> {
		let api_entity = best(self.data_requesting_apis.iter().map(|(data_requesting_api, entity)| (data_requesting_api.cost(&mut self.dyn_system_params, &request), entity))).1;
		for (mut data_requesting_api, entity) in self.data_requesting_apis.iter_mut() {
			if entity == api_entity { continue; }
			data_requesting_api.cancel(&mut self.dyn_system_params, &request.params, requester_node);
		}
		self.data_requesting_apis.get_mut(api_entity).expect("This must exist as it was found in best_node").0.request(&mut self.dyn_system_params, request, requester_node, cache)
	}
	pub fn cancel(&mut self, params: &DT::Params, requester_node: Entity) {
		for (mut data_requesting_api, _entity) in self.data_requesting_apis.iter_mut() {
			data_requesting_api.cancel(&mut self.dyn_system_params, params, requester_node);
		}
	}
}

pub fn downcast<'a, T: SystemParam>(dyn_system_params: &'a mut [DynSystemParam]) -> T
where T::Item<'static, 'static>: SystemParam<Item<'a, 'a> = T> + 'static {
	dyn_system_params.iter_mut().find_map(|dyn_system_param| dyn_system_param.downcast_mut()).expect("Every API is given the param from its dyn_system_param")
}

fn best(costs: impl Iterator<Item = (Option<f32>, Entity)>) -> (f32, Entity) {
	costs
		.filter_map(|(cost, entity)| Some((cost?, entity)))
		.min_by(|a, b| a.0.total_cmp(&b.0))
		.expect("Data request failed to find node to fulfill the request. This should never happen!")
}

// Simple Producing

#[bevy_trait_query::queryable]
pub trait DataProducingNode<DT: DataType> {
	fn cost(&self, request: &Request<DT>) -> Option<f32>; // None if the node can not fulfill the request
	fn request(&mut self, request: Request<DT>);
	fn drop(&mut self, params: &DT::Params);
}

// Simple Producing Data
pub struct DataInfo<DT: DataType> {
	pub dirty_state: DirtyState,
	pub data: Option<DT::Data>,
	pub usages: HashSet<Entity>,
	pub source_entity: Entity,
	retain: bool,
}
struct Waiter {
	allowed_tick: DirtyState,
	cache: bool,
}

#[derive(Resource)]
pub struct DataColumn<DT: DataType> {
	data: HashMap<DT::Params, DataInfo<DT>>,
	data_to_drop: HashMap<DT::Params, HashSet<Entity>>, // this data is no longer requested from this Entity. Either no one wants it or something else provided it.
	finished: HashMap<DT::Params, HashSet<Entity>>,
	waiting: HashMap<DT::Params, HashMap<Entity, Waiter>>,
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

impl<DT: DataType> DataColumn<DT> {
	pub fn get(&self, params: &DT::Params) -> Option<&DataInfo<DT>> { self.data.get(params) }
}

impl<DT: DataType> DataRequestingAPI<DT> for DataColumn<DT> {
	fn dyn_system_param(&self) -> DynParamBuilder<'static> { DynParamBuilder::new::<Query<(One<&mut dyn DataProducingNode<DT>>, Entity)>>(ParamBuilder) }
	fn cost(&self, dyn_system_params: &mut [DynSystemParam], request: &Request<DT>) -> Option<f32> {
		let data_producing_node: Query<(One<&mut dyn DataProducingNode<DT>>, Entity)> = downcast(dyn_system_params);
		Some(best(data_producing_node.iter().map(|(node, entity)| (node.cost(request), entity))).0)
	}
	fn request(&mut self, dyn_system_params: &mut [DynSystemParam], request: Request<DT>, requester_node: Entity, cache: bool) -> Option<DT::Data> {
		if let Some(info) = self.data.get_mut(&request.params) {
			if info.dirty_state.satisfies(&request.allowed_tick) {
				if let Some(data) = &info.data {
					info.usages.insert(requester_node);
					return Some(data.clone());
				}
			}
		}
		let already_requested = self.waiting.get(&request.params).is_some_and(|waiting| waiting.values().any(|waiter| waiter.allowed_tick.satisfies(&request.allowed_tick)));
		// update requester
		self.waiting.entry(request.params.clone()).or_default().insert(requester_node, Waiter { allowed_tick: request.allowed_tick, cache });
		if already_requested { return None; }
		// get node
		let mut data_producing_node: Query<(One<&mut dyn DataProducingNode<DT>>, Entity)> = downcast(dyn_system_params);
		let node_entity = best(data_producing_node.iter().map(|(node, entity)| (node.cost(&request), entity))).1;
		let mut node = data_producing_node.get_mut(node_entity).expect("This must exist as it was found in best_node").0;
		if let Some(sources) = self.data_to_drop.get_mut(&request.params) {
			sources.remove(&node_entity);
		}
		// create queue
		node.request(request);
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

		let has_waiting = self.waiting.contains_key(params);
		let source_to_drop = {
			let Some(info) = self.data.get_mut(params) else { return; };
			info.usages.remove(&requester_node);

			if info.usages.is_empty() && empty_finished && !has_waiting && info.data.take().is_some() {
				info.retain = false;
				Some(info.source_entity)
			} else {
				None
			}
		};

		if let Some(source_entity) = source_to_drop {
			self.data_to_drop.entry(params.clone()).or_default().insert(source_entity);
		}
	}
}

#[derive(SystemParam)]
pub struct DataDirtier<'w, 's, DT: DataType> {
	bile_state_tick: Res<'w, StateTick>,
	data_column: ResMut<'w, DataColumn<DT>>,
	data_producing_nodes: Query<'w, 's, One<&'static mut dyn DataConsumingNode<DT>>>,
}

impl<'w, 's, DT: DataType> DataDirtier<'w, 's, DT> {
	pub fn mark_dirty(&mut self, params: &DT::Params, diff: Option<&DT::Diff>) {
		let Some(info) = self.data_column.data.get_mut(params) else { return; };
		info.dirty_state = DirtyState::Dirty { tick: self.bile_state_tick.current_tick() - 1 /* Was valid on the last tick */ };
		info.usages.retain(|&usage| {
			let Ok(mut data_producing_node) = self.data_producing_nodes.get_mut(usage) else {
				bevy::log::error!(name: "Missing DataConsumingNode", "Usage was not removed for non existent DataConsumingNode {}.", usage);
				return false;
			};
			data_producing_node.dirty_data(params, self.bile_state_tick.current_tick() - 1, diff);
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
		if let Some(existing) = self.data_column.data.get(params) && existing.data.is_some() && existing.dirty_state.is_fresher_than(&dirty_state) {
			return;
		}

		let Some(waiting) = self.data_column.waiting.remove(params) else {
			if self.data_column.data.get(params).is_none_or(|existing| existing.data.is_none() || existing.source_entity != source_entity) {
				self.data_column.data_to_drop.entry(params.clone()).or_default().insert(source_entity);
			}
			return; /* No need to store data thats not wanted */
		};

		let mut want_cache = false;
		let mut still_waiting = HashMap::default();
		for (entity, waiter) in waiting {
			if dirty_state.satisfies(&waiter.allowed_tick) {
				want_cache |= waiter.cache;
				self.data_column.finished.entry(params.clone()).or_default().insert(entity);
			} else {
				still_waiting.insert(entity, waiter);
			}
		}

		match self.data_column.data.entry(params.clone()) {
			std::collections::hash_map::Entry::Occupied(mut entry) => {
				let info = entry.get_mut();
				info.dirty_state = dirty_state;
				info.data = Some(data);
				info.retain |= want_cache;
				if info.source_entity != source_entity {
					let old_source_entity = info.source_entity;
					info.source_entity = source_entity;
					let old_sources = self.data_column.data_to_drop.entry(params.clone()).or_default();
					old_sources.remove(&source_entity);
					old_sources.insert(old_source_entity);
				}
			}
			std::collections::hash_map::Entry::Vacant(entry) => {
				entry.insert(DataInfo { dirty_state, data: Some(data), usages: HashSet::default(), source_entity, retain: want_cache });
				self.data_column.data_to_drop.entry(params.clone()).or_default().remove(&source_entity);
			}
		}

		if self.data_column.finished.get(params).is_none_or(HashSet::is_empty) {
			let source_to_drop = {
				let info = self.data_column.data.get_mut(params).expect("Data was just inserted!");
				if !info.retain && info.data.take().is_some() {
					Some(info.source_entity)
				} else {
					None
				}
			};
			if let Some(source_entity) = source_to_drop {
				self.data_column.data_to_drop.entry(params.clone()).or_default().insert(source_entity);
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

	for new_data_params in std::mem::take(&mut data_column.finished) {
		let source_to_drop = {
			let info = data_column.data.get_mut(&new_data_params.0).expect("Its not new data if its not there!");
			for entity in new_data_params.1 {
				let Some(data) = info.data.clone() else { continue; };
				let Ok(mut data_consuming_node) = data_consuming_nodes.get_mut(entity) else { continue; };
				info.usages.insert(entity);
				data_consuming_node.pass_data(
					&new_data_params.0,
					match info.dirty_state { DirtyState::Dirty { tick } => tick, _ => bile_state_tick.current_tick() },
					data
				);
			}

			if !info.retain && info.data.take().is_some() {
				Some(info.source_entity)
			} else {
				None
			}
		};

		if let Some(source_entity) = source_to_drop {
			data_column.data_to_drop.entry(new_data_params.0).or_default().insert(source_entity);
		}
	}
}

pub fn drop_data<DT: DataType>(
	mut data_column: ResMut<DataColumn<DT>>,
	mut data_producing_nodes: Query<One<&'static mut dyn DataProducingNode<DT>>>,
) {
	if data_column.data_to_drop.is_empty() { return; }

	for (params, sources) in std::mem::take(&mut data_column.data_to_drop) {
		for source_entity in sources {
			let Ok(mut node) = data_producing_nodes.get_mut(source_entity) else { continue; };
			node.drop(&params);
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
		.register_component_as::<dyn DataRequestingAPI<DT>, DataColumn<DT>>();
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
