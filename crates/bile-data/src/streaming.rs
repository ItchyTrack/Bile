use std::{collections::{HashMap, HashSet}, hash::Hash, sync::atomic::{AtomicU64, Ordering}};

use bevy::{
	ecs::system::{DynParamBuilder, DynSystemParam, ParamBuilder, SystemParam},
	prelude::*,
};
use bevy_trait_query::{One, RegisterExt};

use crate::{dyn_system_params::{DynSystemParams, HasDynSystemParam, downcast}, setup::{BileDataSchedule, BileDataSystemSet, DataType, DirtyState, Request, StateTick}};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StreamId(u64);

impl StreamId {
	// Every storage gets every stream, so the ids of all of them have to be different.
	pub fn new() -> Self {
		static NEXT_STREAM_ID: AtomicU64 = AtomicU64::new(0);
		Self(NEXT_STREAM_ID.fetch_add(1, Ordering::Relaxed))
	}
}

#[bevy_trait_query::queryable]
pub trait DataConsumingNode<DT: DataType> {
	fn start_data(&mut self, params: &DT::Params, tick: u64);
	fn push_data(&mut self, params: &DT::Params, data: DT::Data);
	fn finish_data(&mut self, params: &DT::Params);
	fn dirty_data(&mut self, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>);
}

#[bevy_trait_query::queryable]
pub trait DataRequestingAPI<DT: DataType> {
	fn dyn_system_param(&self) -> DynParamBuilder<'static>;
	fn cost(&self, dyn_system_params: &mut [DynSystemParam], request: &Request<DT>) -> Option<f32>;
	fn request(&mut self, dyn_system_params: &mut [DynSystemParam], request: Request<DT>, requester_node: Entity);
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
	pub fn request(&mut self, request: Request<DT>, requester_node: Entity) {
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

#[bevy_trait_query::queryable]
pub trait DataProducingNode<DT: DataType> {
	fn cost(&self, request: &Request<DT>) -> Option<f32>; // None if the node can not fulfill the request
	fn request(&mut self, request: Request<DT>, stream_id: StreamId);
	fn drop(&mut self, params: &DT::Params);
}

type DataProducingNodes<'w, 's, DT> = Query<'w, 's, (One<&'static mut dyn DataProducingNode<DT>>, Entity)>;
pub type DataConsumingNodes<'w, 's, DT> = Query<'w, 's, One<&'static mut dyn DataConsumingNode<DT>>>;

// None if no node can fulfill the request
fn best_node<DT: DataType>(data_producing_node: &DataProducingNodes<DT>, request: &Request<DT>) -> Option<(f32, Entity)> {
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
	fn start(&mut self, source_entity: Entity, params: &DT::Params, stream_id: StreamId, dirty_state: DirtyState);
	fn push(&mut self, source_entity: Entity, params: &DT::Params, stream_id: StreamId, data: &DT::Data);
	fn finish(&mut self, source_entity: Entity, params: &DT::Params, stream_id: StreamId);
	fn dirty_data(&mut self, data_consuming_nodes: &mut DataConsumingNodes<DT>, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>);
	fn uses(&self, params: &DT::Params, source_entity: Entity) -> bool; // a node is only dropped when no storage uses it
	fn drop_data(&mut self) -> HashMap<DT::Params, HashSet<Entity>>; // the nodes this storage no longer uses
}

/// Tracks whichever stream is currently accepted for a given `params`. After
/// `finish` (with `live = false`) it is kept only while something still uses it
/// or waits for it, so that late requesters get the replay and a staler stream
/// is rejected. The cancel of its last user releases it and drops its source.
struct StreamState<DT: DataType> {
	stream_id: StreamId,
	source_entity: Entity,
	dirty_state: DirtyState,
	usages: HashSet<Entity>,
	data: Vec<DT::Data>,
	live: bool,
}

enum StreamEvent<DT: DataType> {
	Start { entities: HashSet<Entity> },
	Push { entities: HashSet<Entity>, data: DT::Data },
	Finish { entities: HashSet<Entity> },
}

#[derive(Resource)]
pub struct DataColumn<DT: DataType> {
	requested: HashMap<DT::Params, StreamId>, // the stream this storage waits for a node to start
	streams: HashMap<DT::Params, StreamState<DT>>,
	data_to_drop: HashMap<DT::Params, HashSet<Entity>>,
	waiting: HashMap<DT::Params, HashMap<Entity, DirtyState>>,
	// Ordered (unlike the other modules' HashMap-based `finished`) because
	// start/push.../finish must be delivered to consumers in that order.
	events: Vec<(DT::Params, StreamEvent<DT>)>,
}

impl<DT: DataType> Default for DataColumn<DT> {
	fn default() -> Self {
		Self {
			requested: HashMap::default(),
			streams: HashMap::default(),
			data_to_drop: HashMap::default(),
			waiting: HashMap::default(),
			events: Vec::default(),
		}
	}
}

impl<DT: DataType> DataRequestingAPI<DT> for DataColumn<DT> {
	fn dyn_system_param(&self) -> DynParamBuilder<'static> { DynParamBuilder::new::<DataProducingNodes<DT>>(ParamBuilder) }
	fn cost(&self, dyn_system_params: &mut [DynSystemParam], request: &Request<DT>) -> Option<f32> {
		best_node(&downcast(dyn_system_params), request).map(|best| best.0)
	}
	fn request(&mut self, dyn_system_params: &mut [DynSystemParam], request: Request<DT>, requester_node: Entity) {
		if let Some(stream) = self.streams.get_mut(&request.params) && stream.dirty_state.satisfies(&request.allowed_tick) {
			stream.usages.insert(requester_node);
			let entities = HashSet::from([requester_node]);
			self.events.push((request.params.clone(), StreamEvent::Start { entities: entities.clone() }));
			self.events.extend(stream.data.iter().map(|data| (request.params.clone(), StreamEvent::Push { entities: entities.clone(), data: data.clone() })));
			if !stream.live {
				self.events.push((request.params, StreamEvent::Finish { entities }));
			}
			return;
		}
		let already_requested = self.waiting.get(&request.params).is_some_and(|waiting| waiting.values().any(|required| required.satisfies(&request.allowed_tick)));
		// update requester
		self.waiting.entry(request.params.clone()).or_default().insert(requester_node, request.allowed_tick);
		if already_requested { return; }
		// get node
		let mut data_producing_node: DataProducingNodes<DT> = downcast(dyn_system_params);
		let node_entity = best_node(&data_producing_node, &request).expect("Data request failed to find node to fulfill the request. This should never happen!").1;
		let mut node = data_producing_node.get_mut(node_entity).expect("This must exist as it was found in best_node").0;
		if let Some(sources) = self.data_to_drop.get_mut(&request.params) {
			sources.remove(&node_entity);
		}
		// issue a fresh stream id and kick off the stream
		let stream_id = StreamId::new();
		self.requested.insert(request.params.clone(), stream_id);
		node.request(request, stream_id);
	}
	fn cancel(&mut self, _dyn_system_params: &mut [DynSystemParam], params: &DT::Params, requester_node: Entity) {
		if let Some(waiting) = self.waiting.get_mut(params) {
			waiting.remove(&requester_node);
			if waiting.is_empty() {
				self.waiting.remove(params);
			}
		}

		let Some(stream) = self.streams.get_mut(params) else { return; };
		stream.usages.remove(&requester_node);
		let source_entity = stream.source_entity;
		if stream.usages.is_empty() && !self.waiting.contains_key(params) {
			self.data_to_drop.entry(params.clone()).or_default().insert(source_entity);
			self.streams.remove(params);
		}
	}
}

#[derive(SystemParam)]
pub struct DataDirtier<'w, 's, DT: DataType> {
	bile_state_tick: Res<'w, StateTick>,
	data_storages: Query<'w, 's, One<&'static mut dyn DataStorage<DT>>>,
	data_consuming_nodes: DataConsumingNodes<'w, 's, DT>,
}

impl<'w, 's, DT: DataType> DataDirtier<'w, 's, DT> {
	pub fn mark_dirty(&mut self, params: &DT::Params, diff: Option<&DT::Diff>) {
		let last_clean_tick = self.bile_state_tick.update();
		for mut data_storage in &mut self.data_storages {
			data_storage.dirty_data(&mut self.data_consuming_nodes, params, last_clean_tick, diff);
		}
	}
}

#[derive(SystemParam)]
pub struct DataPasser<'w, 's, DT: DataType> {
	data_storages: Query<'w, 's, One<&'static mut dyn DataStorage<DT>>>,
}

impl<'w, 's, DT: DataType> DataPasser<'w, 's, DT> {
	/// Producer calls this once, before any `push`.
	pub fn start(&mut self, source_entity: Entity, params: &DT::Params, stream_id: StreamId, dirty_state: DirtyState) {
		for mut data_storage in &mut self.data_storages {
			data_storage.start(source_entity, params, stream_id, dirty_state);
		}
	}

	/// Producer calls this any number of times between `start` and `finish`.
	pub fn push(&mut self, source_entity: Entity, params: &DT::Params, stream_id: StreamId, data: DT::Data) {
		for mut data_storage in &mut self.data_storages {
			data_storage.push(source_entity, params, stream_id, &data);
		}
	}

	/// Producer calls this once to close out the stream.
	pub fn finish(&mut self, source_entity: Entity, params: &DT::Params, stream_id: StreamId) {
		for mut data_storage in &mut self.data_storages {
			data_storage.finish(source_entity, params, stream_id);
		}
	}
}

impl<DT: DataType> DataStorage<DT> for DataColumn<DT> {
	fn start(&mut self, source_entity: Entity, params: &DT::Params, stream_id: StreamId, dirty_state: DirtyState) {
		if self.requested.get(params) != Some(&stream_id) { return; } // the stream of another storage
		self.requested.remove(params);
		if let Some(existing) = self.streams.get(params) && existing.dirty_state.is_fresher_than(&dirty_state) {
			// something fresher is already active or already finished here; reject outright
			self.data_to_drop.entry(params.clone()).or_default().insert(source_entity);
			return;
		}

		let Some(waiting) = self.waiting.remove(params) else {
			self.data_to_drop.entry(params.clone()).or_default().insert(source_entity);
			return; /* No one wants this anymore */
		};

		if let Some(old) = self.streams.remove(params) {
			// supersede whatever was there, live or not; if it was still live, tell it to stop too
			if old.live || old.source_entity != source_entity {
				self.data_to_drop.entry(params.clone()).or_default().insert(old.source_entity);
			}
		}

		let mut usages = HashSet::default();
		let mut still_waiting = HashMap::default();
		for (entity, required) in waiting {
			if dirty_state.satisfies(&required) {
				usages.insert(entity);
			} else {
				still_waiting.insert(entity, required);
			}
		}
		if !still_waiting.is_empty() {
			self.waiting.insert(params.clone(), still_waiting);
		}
		if !usages.is_empty() {
			self.events.push((params.clone(), StreamEvent::Start { entities: usages.clone() }));
		}

		self.streams.insert(params.clone(), StreamState { stream_id, source_entity, dirty_state, usages, data: Vec::new(), live: true });
	}
	fn push(&mut self, source_entity: Entity, params: &DT::Params, stream_id: StreamId, data: &DT::Data) {
		let Some(stream) = self.streams.get_mut(params) else { return; };
		if stream.stream_id != stream_id || !stream.live || stream.source_entity != source_entity { return; } // stale stream, ignore
		stream.data.push(data.clone());
		if stream.usages.is_empty() { return; }
		let usages = stream.usages.clone();
		self.events.push((params.clone(), StreamEvent::Push { entities: usages, data: data.clone() }));
	}
	fn finish(&mut self, source_entity: Entity, params: &DT::Params, stream_id: StreamId) {
		let Some(stream) = self.streams.get_mut(params) else { return; };
		if stream.stream_id != stream_id || !stream.live || stream.source_entity != source_entity { return; } // stale stream, ignore
		stream.live = false;
		let entities = stream.usages.clone();
		if !entities.is_empty() {
			self.events.push((params.clone(), StreamEvent::Finish { entities }));
		}
	}
	fn dirty_data(&mut self, data_consuming_nodes: &mut DataConsumingNodes<DT>, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>) {
		let Some(stream) = self.streams.get_mut(params) else { return; };
		stream.dirty_state = DirtyState::Dirty { tick: last_clean_tick };
		stream.usages.retain(|&usage| {
			let Ok(mut node) = data_consuming_nodes.get_mut(usage) else {
				bevy::log::error!(name: "Missing DataConsumingNode", "Usage was not removed for non existent DataConsumingNode {}.", usage);
				return false;
			};
			node.dirty_data(params, last_clean_tick, diff);
			true
		})
	}
	fn uses(&self, params: &DT::Params, source_entity: Entity) -> bool {
		self.waiting.contains_key(params) || self.streams.get(params).is_some_and(|stream| stream.source_entity == source_entity)
	}
	fn drop_data(&mut self) -> HashMap<DT::Params, HashSet<Entity>> { std::mem::take(&mut self.data_to_drop) }
}

pub fn pass_data<DT: DataType>(
	bile_state_tick: Res<StateTick>,
	mut data_column: ResMut<DataColumn<DT>>,
	mut data_consuming_nodes: DataConsumingNodes<DT>,
) {
	if data_column.events.is_empty() { return; }

	for (params, event) in std::mem::take(&mut data_column.events) {
		match event {
			StreamEvent::Start { entities } => {
				let tick = match data_column.streams.get(&params).map(|s| s.dirty_state) {
					Some(DirtyState::Dirty { tick }) => tick,
					_ => bile_state_tick.current_tick(),
				};
				for entity in entities {
					let Ok(mut node) = data_consuming_nodes.get_mut(entity) else { continue; };
					node.start_data(&params, tick);
				}
			}
			StreamEvent::Push { entities, data } => {
				for entity in entities {
					let Ok(mut node) = data_consuming_nodes.get_mut(entity) else { continue; };
					node.push_data(&params, data.clone());
				}
			}
			StreamEvent::Finish { entities } => {
				for entity in entities {
					let Ok(mut node) = data_consuming_nodes.get_mut(entity) else { continue; };
					node.finish_data(&params);
				}
			}
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

pub trait BileRegisterStreaming {
	fn register_producer_streaming<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + DataProducingNode<DT>;
	fn register_consumer_streaming<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + DataConsumingNode<DT>;
}

impl BileRegisterStreaming for App {
	fn register_producer_streaming<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + DataProducingNode<DT>,
	{
		self.register_component_as::<dyn DataProducingNode<DT>, C>();
		setup_bevy_resources::<DT>(self);
		self
	}

	fn register_consumer_streaming<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + DataConsumingNode<DT>,
	{
		self.register_component_as::<dyn DataConsumingNode<DT>, C>();
		setup_bevy_resources::<DT>(self);
		self
	}
}
