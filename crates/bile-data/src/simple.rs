use std::collections::{HashMap, HashSet};

use bevy::{
	ecs::system::{DynParamBuilder, DynSystemParam, ParamBuilder, SystemParam},
	prelude::*,
};
use bevy_trait_query::{One, RegisterExt};

use crate::{diff::DiffApplyingNode, default::{self, BileDataSchedule, BileDataSystemSet, DataRequestingAPI, DataType, DirtyState, Request, StateTick, StreamingDataType, best, downcast}, streaming};

// Simple Consuming

#[bevy_trait_query::queryable]
pub trait DataConsumingNode<DT: DataType> {
	fn pass_data(&mut self, params: &DT::Params, tick: u64, data: DT::Data);
	fn dirty_data(&mut self, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>);
}

pub type DataRequester<'w, 's, DT> = default::DataRequester<'w, 's, Request<DT>>;

// Simple Producing

#[bevy_trait_query::queryable]
pub trait DataProducingNode<DT: DataType> {
	fn cost(&self, request: &Request<DT>) -> Option<f32>; // None if the node can not fulfill the request
	fn request(&mut self, request: Request<DT>);
	fn drop(&mut self, params: &DT::Params);
}

pub type DataProducingNodes<'w, 's, DT> = Query<'w, 's, (One<&'static mut dyn DataProducingNode<DT>>, Entity)>;
type DiffApplyingNodes<'w, 's, DT> = Query<'w, 's, (One<&'static mut dyn DiffApplyingNode<DT>>, Entity)>;
type DataNodes<'w, 's, DT> = ParamSet<'w, 's, (DataProducingNodes<'static, 'static, DT>, DiffApplyingNodes<'static, 'static, DT>)>;

// Simple Producing Data
pub struct DataInfo<DT: DataType> {
	pub dirty_state: DirtyState,
	pub data: Option<DT::Data>,
	pub usages: HashSet<Entity>,
	pub source_entity: Entity,
	chain: Vec<Entity>, // earlier sources, whose out of date data is not stored
	diffs: Option<Vec<DT::Diff>>, // every dirty since the data was made. None if one had no diff
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
		let node_entity = best(data_producing_node.iter().map(|(node, entity)| (node.cost(&request), entity))).expect("Data request failed to find node to fulfill the request. This should never happen!").1;
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
			let sources = self.data_to_drop.entry(params.clone()).or_default();
			sources.insert(info.source_entity);
			sources.extend(info.chain.drain(..));
			self.data.remove(params);
		}
	}
	fn uses(&self, params: &DT::Params, source_entity: Entity) -> bool {
		self.waiting.contains_key(params) || self.data.get(params).is_some_and(|info| info.source_entity == source_entity || info.chain.contains(&source_entity))
	}
	fn mark_dirty(&mut self, data_consuming_nodes: &mut Query<One<&'static mut dyn DataConsumingNode<DT>>>, params: &DT::Params, tick: u64, diff: Option<&DT::Diff>) {
		let Some(info) = self.data.get_mut(params) else { return; };
		info.dirty_state = DirtyState::Dirty { tick };
		match (&mut info.diffs, diff) {
			(Some(diffs), Some(diff)) => diffs.push(diff.clone()),
			_ => info.diffs = None, // There may never be a dirty inbetween 2 diffs
		}
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

		let diffs = (CACHE && matches!(dirty_state, DirtyState::UpToDate)).then(Vec::new);
		let dirty_state = match self.data.entry(params.clone()) {
			std::collections::hash_map::Entry::Occupied(mut entry) => {
				let info = entry.get_mut();
				info.data = CACHE.then(|| data.clone());
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
				entry.insert(DataInfo { dirty_state, data: CACHE.then(|| data.clone()), usages: HashSet::default(), source_entity, chain: Vec::new(), diffs });
				self.data_to_drop.entry(params.clone()).or_default().remove(&source_entity);
				dirty_state
			}
		};

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

impl<DT: DataType> DataColumn<DT> {
	fn best_diff(&self, diff_applying_node: &DiffApplyingNodes<DT>, request: &Request<DT>) -> Option<(f32, Entity)> {
		let info = self.data.get(&request.params).filter(|info| info.data.is_some() && !info.dirty_state.satisfies(&request.allowed_tick))?;
		let diffs = info.diffs.as_ref().filter(|diffs| !diffs.is_empty())?;
		best(diff_applying_node.iter().map(|(node, entity)| (node.cost(request, diffs), entity)))
	}
}

impl<DT: DataType> DataRequestingAPI<Request<DT>> for DataColumn<DT> {
	fn dyn_system_param(&self) -> DynParamBuilder<'static> { DynParamBuilder::new::<DataNodes<DT>>(ParamBuilder) }
	fn cost(&self, dyn_system_params: &mut [DynSystemParam], request: &Request<DT>) -> Option<f32> {
		let mut data_nodes: DataNodes<DT> = downcast(dyn_system_params);
		let diff_cost = self.best_diff(&data_nodes.p1(), request).map(|best| best.0);
		let node_cost = best(data_nodes.p0().iter().map(|(node, entity)| (node.cost(request), entity))).map(|best| best.0);
		[diff_cost, node_cost].into_iter().flatten().reduce(f32::min)
	}
	fn request(&mut self, dyn_system_params: &mut [DynSystemParam], request: Request<DT>, requester_node: Entity) -> Option<DT::Data> {
		let mut data_nodes: DataNodes<DT> = downcast(dyn_system_params);
		let node_cost = best(data_nodes.p0().iter().map(|(node, entity)| (node.cost(&request), entity))).map(|best| best.0);
		if let Some((cost, node_entity)) = self.best_diff(&data_nodes.p1(), &request) && node_cost.is_none_or(|node_cost| cost < node_cost) {
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
		self.request(&mut data_nodes.p0(), request, requester_node)
	}
	fn cancel(&mut self, _dyn_system_params: &mut [DynSystemParam], params: &DT::Params, requester_node: Entity) { self.cancel(params, requester_node); }
}

// A stream from simple producers is the one data that has the params of the stream.

enum Streamed<DT: DataType> {
	Pass(DT::Params, u64, DT::Data),
	Dirty(DT::Params, u64, Option<DT::Diff>),
}

#[derive(Component)]
struct StreamedData<SDT: StreamingDataType>(Vec<Streamed<SDT::DataType>>);

impl<DT: DataType, SDT: StreamingDataType<DataType = DT>> DataConsumingNode<DT> for StreamedData<SDT> {
	fn pass_data(&mut self, params: &DT::Params, tick: u64, data: DT::Data) { self.0.push(Streamed::Pass(params.clone(), tick, data)); }
	fn dirty_data(&mut self, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>) { self.0.push(Streamed::Dirty(params.clone(), last_clean_tick, diff.cloned())); }
}

impl<DT: DataType, SDT: StreamingDataType<DataType = DT, Params = DT::Params>> DataRequestingAPI<streaming::Request<SDT>> for DataColumn<DT> {
	fn dyn_system_param(&self) -> DynParamBuilder<'static> { DynParamBuilder::new::<(DataProducingNodes<DT>, Commands)>(ParamBuilder) }
	fn cost(&self, dyn_system_params: &mut [DynSystemParam], request: &streaming::Request<SDT>) -> Option<f32> {
		let (data_producing_node, _): (DataProducingNodes<DT>, Commands) = downcast(dyn_system_params);
		let request = Request::new(request.params.clone(), request.allowed_tick);
		best(data_producing_node.iter().map(|(node, entity)| (node.cost(&request), entity))).map(|best| best.0)
	}
	fn request(&mut self, dyn_system_params: &mut [DynSystemParam], request: streaming::Request<SDT>, requester_node: Entity) {
		let (mut data_producing_node, mut commands): (DataProducingNodes<DT>, Commands) = downcast(dyn_system_params);
		commands.entity(requester_node).entry::<StreamedData<SDT>>().or_insert_with(|| StreamedData(Vec::new()));
		if let Some(data) = self.request(&mut data_producing_node, Request::new(request.params.clone(), request.allowed_tick), requester_node) {
			self.finished.entry(request.params).or_insert_with(|| (HashSet::default(), data)).0.insert(requester_node);
		}
	}
	fn cancel(&mut self, _dyn_system_params: &mut [DynSystemParam], params: &DT::Params, requester_node: Entity) { self.cancel(params, requester_node); }
}

fn pass_streamed_data<SDT: StreamingDataType<Params = <<SDT as StreamingDataType>::DataType as DataType>::Params>>(
	mut data_consuming_nodes: Query<(&mut StreamedData<SDT>, One<&mut dyn streaming::DataConsumingNode<SDT>>), Changed<StreamedData<SDT>>>,
) {
	for (mut streamed_data, mut data_consuming_node) in &mut data_consuming_nodes {
		for streamed in streamed_data.bypass_change_detection().0.drain(..) {
			match streamed {
				Streamed::Pass(params, tick, data) => {
					data_consuming_node.start_data(&params, tick);
					data_consuming_node.push_data(&params, &params, data);
					data_consuming_node.finish_data(&params);
				}
				Streamed::Dirty(params, last_clean_tick, diff) => data_consuming_node.dirty_data(&params, last_clean_tick, diff.as_ref()),
			}
		}
	}
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
	mut data_nodes: DataNodes<DT>,
) {
	let mut data_to_drop = std::mem::take(&mut data_column.data_to_drop);
	for (params, sources) in std::mem::take(&mut uncached_data_column.data_to_drop) {
		data_to_drop.entry(params).or_default().extend(sources);
	}
	for (params, sources) in data_to_drop {
		for source_entity in sources {
			if data_column.uses(&params, source_entity) || uncached_data_column.uses(&params, source_entity) { continue; }
			if let Ok((mut node, _)) = data_nodes.p0().get_mut(source_entity) { node.drop(&params); }
			if let Ok((mut node, _)) = data_nodes.p1().get_mut(source_entity) { node.drop(&params); }
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
		.register_component_as::<dyn DataRequestingAPI<Request<DT>>, DataColumn<DT>>();
}

pub trait BileRegisterSimple {
	fn register_producer_simple<DT, C>(&mut self) -> &mut Self
    where DT: DataType, C: Component + DataProducingNode<DT>;
	fn register_consumer_simple<DT, C>(&mut self) -> &mut Self
    where DT: DataType, C: Component + DataConsumingNode<DT>;
	fn register_streaming_simple<SDT>(&mut self) -> &mut Self
    where SDT: StreamingDataType<Params = <<SDT as StreamingDataType>::DataType as DataType>::Params>;
	fn register_diff_applier_simple<DT, C>(&mut self) -> &mut Self
    where DT: DataType, C: Component + DiffApplyingNode<DT>;
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

    fn register_streaming_simple<SDT>(&mut self) -> &mut Self
    where SDT: StreamingDataType<Params = <<SDT as StreamingDataType>::DataType as DataType>::Params>,
    {
        self.register_component_as::<dyn DataRequestingAPI<streaming::Request<SDT>>, DataColumn<SDT::DataType>>()
			.register_component_as::<dyn DataConsumingNode<SDT::DataType>, StreamedData<SDT>>()
			.add_systems(BileDataSchedule, pass_streamed_data::<SDT>.in_set(BileDataSystemSet::PassData).after(pass_data::<SDT::DataType, true>));
		setup_bevy_resources::<SDT::DataType>(self);
        self
    }

    fn register_diff_applier_simple<DT, C>(&mut self) -> &mut Self
    where DT: DataType, C: Component + DiffApplyingNode<DT>,
    {
        self.register_component_as::<dyn DiffApplyingNode<DT>, C>()
    }
}
