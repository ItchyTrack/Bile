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
	fn request(&mut self, dyn_system_params: &mut [DynSystemParam], request: Request<DT>, requester_node: Entity) -> Option<DT::Data>;
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
	pub fn request(&mut self, request: Request<DT>, requester_node: Entity) -> Option<DT::Data> {
		let api_entity = best(self.data_requesting_apis.iter().map(|(data_requesting_api, entity)| (data_requesting_api.cost(&mut self.dyn_system_params, &request), entity))).1;
		for (mut data_requesting_api, entity) in self.data_requesting_apis.iter_mut() {
			if entity == api_entity { continue; }
			data_requesting_api.cancel(&mut self.dyn_system_params, &request.params, requester_node);
		}
		self.data_requesting_apis.get_mut(api_entity).expect("This must exist as it was found in best_node").0.request(&mut self.dyn_system_params, request, requester_node)
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

pub type DataProducingNodes<'w, 's, DT> = Query<'w, 's, (One<&'static mut dyn DataProducingNode<DT>>, Entity)>;

// Simple Producing Data
pub struct DataInfo<DT: DataType> {
	pub dirty_state: DirtyState,
	pub data: Option<DT::Data>, // None if the column does not cache
	pub usages: HashSet<Entity>,
	pub source_entity: Entity,
}

// With `CACHE` false the column only keeps track of the data and the requester has to store it.
#[derive(Resource)]
pub struct DataColumn<DT: DataType, const CACHE: bool = true> {
	data: HashMap<DT::Params, DataInfo<DT>>,
	data_to_drop: HashMap<DT::Params, HashSet<Entity>>, // this data is no longer requested from this Entity. Either no one wants it or something else provided it.
	finished: HashMap<DT::Params, (HashSet<Entity>, DT::Data)>,
	waiting: HashMap<DT::Params, HashMap<Entity, DirtyState>>,
}

impl<DT: DataType, const CACHE: bool> Default for DataColumn<DT, CACHE> {
	fn default() -> Self {
		Self {
			data: HashMap::default(),
			data_to_drop: HashMap::default(),
			finished: HashMap::default(),
			waiting: HashMap::default(),
		}
	}
}

impl<DT: DataType, const CACHE: bool> DataColumn<DT, CACHE> {
	pub fn get(&self, params: &DT::Params) -> Option<&DataInfo<DT>> { self.data.get(params) }
	pub fn request(&mut self, data_producing_node: &mut DataProducingNodes<DT>, request: Request<DT>, requester_node: Entity) -> Option<DT::Data> {
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
		let node_entity = best(data_producing_node.iter().map(|(node, entity)| (node.cost(&request), entity))).1;
		let mut node = data_producing_node.get_mut(node_entity).expect("This must exist as it was found in best_node").0;
		if let Some(sources) = self.data_to_drop.get_mut(&request.params) {
			sources.remove(&node_entity);
		}
		// create queue
		node.request(request);
		None
	}
	pub fn cancel(&mut self, params: &DT::Params, requester_node: Entity) {
		if let Some(waiting) = self.waiting.get_mut(params) {
			waiting.remove(&requester_node);
			if waiting.is_empty() {
				self.waiting.remove(params);
			}
		}
		let mut empty_finished = true;
		if let Some((finished, _)) = self.finished.get_mut(params) {
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
			self.data_to_drop.entry(params.clone()).or_default().insert(info.source_entity);
			self.data.remove(params);
		}
	}
	fn uses(&self, params: &DT::Params, source_entity: Entity) -> bool {
		self.waiting.contains_key(params) || self.data.get(params).is_some_and(|info| info.source_entity == source_entity)
	}
	fn mark_dirty(&mut self, data_consuming_nodes: &mut Query<One<&'static mut dyn DataConsumingNode<DT>>>, params: &DT::Params, tick: u64, diff: Option<&DT::Diff>) {
		let Some(info) = self.data.get_mut(params) else { return; };
		info.dirty_state = DirtyState::Dirty { tick };
		info.usages.retain(|&usage| {
			let Ok(mut data_consuming_node) = data_consuming_nodes.get_mut(usage) else {
				bevy::log::error!(name: "Missing DataConsumingNode", "Usage was not removed for non existent DataConsumingNode {}.", usage);
				return false;
			};
			data_consuming_node.dirty_data(params, tick, diff);
			true
		})
	}
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

		match self.data.entry(params.clone()) {
			std::collections::hash_map::Entry::Occupied(mut entry) => {
				let info = entry.get_mut();
				info.dirty_state = dirty_state;
				info.data = CACHE.then(|| data.clone());
				if info.source_entity != source_entity {
					let old_source_entity = info.source_entity;
					info.source_entity = source_entity;
					let old_sources = self.data_to_drop.entry(params.clone()).or_default();
					old_sources.remove(&source_entity);
					old_sources.insert(old_source_entity);
				}
			}
			std::collections::hash_map::Entry::Vacant(entry) => {
				entry.insert(DataInfo { dirty_state, data: CACHE.then(|| data.clone()), usages: HashSet::default(), source_entity });
				self.data_to_drop.entry(params.clone()).or_default().remove(&source_entity);
			}
		}

		let finished = self.finished.entry(params.clone()).or_insert_with(|| (HashSet::default(), data.clone()));
		finished.1 = data.clone();
		let mut still_waiting = HashMap::default();
		for (entity, required) in waiting {
			if dirty_state.satisfies(&required) {
				finished.0.insert(entity);
			} else {
				still_waiting.insert(entity, required);
			}
		}
		if !still_waiting.is_empty() {
			self.waiting.insert(params.clone(), still_waiting);
		}
	}
}

impl<DT: DataType> DataRequestingAPI<DT> for DataColumn<DT> {
	fn dyn_system_param(&self) -> DynParamBuilder<'static> { DynParamBuilder::new::<DataProducingNodes<DT>>(ParamBuilder) }
	fn cost(&self, dyn_system_params: &mut [DynSystemParam], request: &Request<DT>) -> Option<f32> {
		let data_producing_node: DataProducingNodes<DT> = downcast(dyn_system_params);
		Some(best(data_producing_node.iter().map(|(node, entity)| (node.cost(request), entity))).0)
	}
	fn request(&mut self, dyn_system_params: &mut [DynSystemParam], request: Request<DT>, requester_node: Entity) -> Option<DT::Data> {
		self.request(&mut downcast(dyn_system_params), request, requester_node)
	}
	fn cancel(&mut self, _dyn_system_params: &mut [DynSystemParam], params: &DT::Params, requester_node: Entity) { self.cancel(params, requester_node); }
}

#[derive(SystemParam)]
pub struct DataDirtier<'w, 's, DT: DataType> {
	bile_state_tick: Res<'w, StateTick>,
	data_column: ResMut<'w, DataColumn<DT>>,
	uncached_data_column: ResMut<'w, DataColumn<DT, false>>,
	data_consuming_nodes: Query<'w, 's, One<&'static mut dyn DataConsumingNode<DT>>>,
}

impl<'w, 's, DT: DataType> DataDirtier<'w, 's, DT> {
	pub fn mark_dirty(&mut self, params: &DT::Params, diff: Option<&DT::Diff>) {
		let tick = self.bile_state_tick.current_tick() - 1; /* Was valid on the last tick */
		self.data_column.mark_dirty(&mut self.data_consuming_nodes, params, tick, diff);
		self.uncached_data_column.mark_dirty(&mut self.data_consuming_nodes, params, tick, diff);
	}
}

#[derive(SystemParam)]
pub struct DataPasser<'w, DT: DataType> {
	data_column: ResMut<'w, DataColumn<DT>>,
	uncached_data_column: ResMut<'w, DataColumn<DT, false>>,
}

impl<'w, DT: DataType> DataPasser<'w, DT> {
	pub fn pass_data(&mut self, source_entity: Entity, params: &DT::Params, dirty_state: DirtyState, data: DT::Data) {
		self.data_column.pass_data(source_entity, params, dirty_state, &data);
		self.uncached_data_column.pass_data(source_entity, params, dirty_state, &data);
	}
}

pub fn pass_data<DT: DataType, const CACHE: bool>(
	bile_state_tick: Res<StateTick>,
	mut data_column: ResMut<DataColumn<DT, CACHE>>,
	mut data_consuming_nodes: Query<One<&'static mut dyn DataConsumingNode<DT>>>,
) {
	if data_column.finished.is_empty() { return; }

	for (params, (entities, data)) in std::mem::take(&mut data_column.finished) {
		let info = data_column.data.get_mut(&params).expect("Its not new data if its not there!");
		for entity in entities {
			let Ok(mut data_consuming_node) = data_consuming_nodes.get_mut(entity) else { continue; };
			info.usages.insert(entity);
			data_consuming_node.pass_data(
				&params,
				match info.dirty_state { DirtyState::Dirty { tick } => tick, _ => bile_state_tick.current_tick() },
				data.clone()
			);
		}
	}
}

pub fn drop_data<DT: DataType>(
	mut data_column: ResMut<DataColumn<DT>>,
	mut uncached_data_column: ResMut<DataColumn<DT, false>>,
	mut data_producing_nodes: Query<One<&'static mut dyn DataProducingNode<DT>>>,
) {
	let mut data_to_drop = std::mem::take(&mut data_column.data_to_drop);
	for (params, sources) in std::mem::take(&mut uncached_data_column.data_to_drop) {
		data_to_drop.entry(params).or_default().extend(sources);
	}
	for (params, sources) in data_to_drop {
		for source_entity in sources {
			if data_column.uses(&params, source_entity) || uncached_data_column.uses(&params, source_entity) { continue; }
			let Ok(mut node) = data_producing_nodes.get_mut(source_entity) else { continue; };
			node.drop(&params);
		}
	}
}

fn setup_bevy_resources<DT: DataType>(app: &mut App) {
	if app.world().contains_resource::<DataColumn<DT>>() { return; }
	app
		.add_systems(BileDataSchedule, (
			(pass_data::<DT, true>, pass_data::<DT, false>).in_set(BileDataSystemSet::PassData),
			drop_data::<DT>.in_set(BileDataSystemSet::DropData),
		))
		.init_resource::<DataColumn<DT>>()
		.init_resource::<DataColumn<DT, false>>()
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
