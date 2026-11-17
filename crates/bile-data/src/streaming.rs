use std::{collections::{HashMap, HashSet}, hash::Hash, sync::atomic::{AtomicU64, Ordering}};

use bevy::{
	ecs::system::{DynParamBuilder, DynSystemParam, ParamBuilder, SystemParam},
	prelude::*,
};
use bevy_trait_query::{One, RegisterExt};

use crate::{default::{BileDataSchedule, BileDataSystemSet, DirtyState, StateTick}, dyn_system_params::{DynSystemParams, HasDynSystemParam, downcast}};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StreamId(u64);

impl StreamId {
	// Every storage gets every stream, so the ids of all of them have to be different.
	pub fn new() -> Self {
		static NEXT_STREAM_ID: AtomicU64 = AtomicU64::new(0);
		Self(NEXT_STREAM_ID.fetch_add(1, Ordering::Relaxed))
	}
}

pub trait StreamingDataType: 'static {
	type Params: Clone + Hash + PartialEq + Eq + Send + Sync + 'static;
	type Data: Clone + Send + Sync + 'static;
	type Diff: Clone + Send + Sync + 'static;
}

pub struct Request<SDT: StreamingDataType> {
	pub params: SDT::Params,
	pub allowed_tick: DirtyState, // UpToDate -> Only up to date data; Dirty(tick) -> Reject data before tick
}

#[bevy_trait_query::queryable]
pub trait DataConsumingNode<SDT: StreamingDataType> {
	fn start_data(&mut self, params: &SDT::Params, tick: u64);
	fn push_data(&mut self, params: &SDT::Params, data: SDT::Data);
	fn finish_data(&mut self, params: &SDT::Params);
	fn dirty_data(&mut self, params: &SDT::Params, last_clean_tick: u64, diff: Option<&SDT::Diff>);
}

#[bevy_trait_query::queryable]
pub trait DataRequestingAPI<SDT: StreamingDataType> {
	fn dyn_system_param(&self) -> DynParamBuilder<'static>;
	fn cost(&self, dyn_system_params: &mut [DynSystemParam], request: &Request<SDT>) -> Option<f32>;
	fn request(&mut self, dyn_system_params: &mut [DynSystemParam], request: Request<SDT>, requester_node: Entity);
	fn cancel(&mut self, dyn_system_params: &mut [DynSystemParam], params: &SDT::Params, requester_node: Entity);
}

#[derive(SystemParam)]
pub struct DataRequester<'w, 's, SDT: StreamingDataType> {
	data_requesting_apis: Query<'w, 's, (One<&'static mut dyn DataRequestingAPI<SDT>>, Entity)>,
	dyn_system_params: DynSystemParams<'w, 's, dyn DataRequestingAPI<SDT>>,
}

impl<SDT: StreamingDataType> HasDynSystemParam for dyn DataRequestingAPI<SDT> {
	fn dyn_system_param(&self) -> DynParamBuilder<'static> { DataRequestingAPI::dyn_system_param(self) }
}

impl<'w, 's, SDT: StreamingDataType> DataRequester<'w, 's, SDT> {
	pub fn request(&mut self, request: Request<SDT>, requester_node: Entity) {
		let api_entity = Self::best_api(&self.data_requesting_apis, &mut self.dyn_system_params.0, &request);
		for (mut data_requesting_api, entity) in self.data_requesting_apis.iter_mut() {
			if entity == api_entity { continue; }
			data_requesting_api.cancel(&mut self.dyn_system_params.0, &request.params, requester_node);
		}
		self.data_requesting_apis.get_mut(api_entity).expect("This must exist as it was found in best_node").0.request(&mut self.dyn_system_params.0, request, requester_node)
	}
	pub fn cancel(&mut self, params: &SDT::Params, requester_node: Entity) {
		for (mut data_requesting_api, _entity) in self.data_requesting_apis.iter_mut() {
			data_requesting_api.cancel(&mut self.dyn_system_params.0, params, requester_node);
		}
	}
	fn best_api(data_requesting_apis: &Query<'w, 's, (One<&'static mut dyn DataRequestingAPI<SDT>>, Entity)>, dyn_system_params: &mut [DynSystemParam], request: &Request<SDT>) -> Entity {
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
pub trait DataProducingNode<SDT: StreamingDataType> {
	fn cost(&self, request: &Request<SDT>) -> Option<f32>; // None if the node can not fulfill the request
	fn request(&mut self, request: Request<SDT>, stream_id: StreamId);
	fn drop(&mut self, params: &SDT::Params);
}

type DataProducingNodes<'w, 's, SDT> = Query<'w, 's, (One<&'static mut dyn DataProducingNode<SDT>>, Entity)>;
pub type DataConsumingNodes<'w, 's, SDT> = Query<'w, 's, One<&'static mut dyn DataConsumingNode<SDT>>>;

// None if no node can fulfill the request
fn best_node<SDT: StreamingDataType>(data_producing_node: &DataProducingNodes<SDT>, request: &Request<SDT>) -> Option<(f32, Entity)> {
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
pub trait DataStorage<SDT: StreamingDataType> {
	fn start(&mut self, source_entity: Entity, params: &SDT::Params, stream_id: StreamId, dirty_state: DirtyState);
	fn push(&mut self, source_entity: Entity, params: &SDT::Params, stream_id: StreamId, data: &SDT::Data);
	fn finish(&mut self, source_entity: Entity, params: &SDT::Params, stream_id: StreamId);
	fn dirty_data(&mut self, data_consuming_nodes: &mut DataConsumingNodes<SDT>, params: &SDT::Params, last_clean_tick: u64, diff: Option<&SDT::Diff>);
	fn uses(&self, params: &SDT::Params, source_entity: Entity) -> bool; // a node is only dropped when no storage uses it
	fn drop_data(&mut self) -> HashMap<SDT::Params, HashSet<Entity>>; // the nodes this storage no longer uses
}

/// Tracks whichever stream is currently accepted for a given `params`. Kept
/// around even after `finish` (with `live = false`) purely so a later, staler
/// stream has something fresher to be compared against and rejected by.
struct StreamState<SDT: StreamingDataType> {
	stream_id: StreamId,
	source_entity: Entity,
	dirty_state: DirtyState,
	usages: HashSet<Entity>,
	data: Vec<SDT::Data>,
	live: bool,
}

enum StreamEvent<SDT: StreamingDataType> {
	Start { entities: HashSet<Entity> },
	Push { entities: HashSet<Entity>, data: SDT::Data },
	Finish { entities: HashSet<Entity> },
}

#[derive(Resource)]
pub struct DataColumn<SDT: StreamingDataType> {
	requested: HashMap<SDT::Params, StreamId>, // the stream this storage waits for a node to start
	streams: HashMap<SDT::Params, StreamState<SDT>>,
	data_to_drop: HashMap<SDT::Params, HashSet<Entity>>,
	waiting: HashMap<SDT::Params, HashMap<Entity, DirtyState>>,
	// Ordered (unlike the other modules' HashMap-based `finished`) because
	// start/push.../finish must be delivered to consumers in that order.
	events: Vec<(SDT::Params, StreamEvent<SDT>)>,
}

impl<SDT: StreamingDataType> Default for DataColumn<SDT> {
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

impl<SDT: StreamingDataType> DataRequestingAPI<SDT> for DataColumn<SDT> {
	fn dyn_system_param(&self) -> DynParamBuilder<'static> { DynParamBuilder::new::<DataProducingNodes<SDT>>(ParamBuilder) }
	fn cost(&self, dyn_system_params: &mut [DynSystemParam], request: &Request<SDT>) -> Option<f32> {
		best_node(&downcast(dyn_system_params), request).map(|best| best.0)
	}
	fn request(&mut self, dyn_system_params: &mut [DynSystemParam], request: Request<SDT>, requester_node: Entity) {
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
		let mut data_producing_node: DataProducingNodes<SDT> = downcast(dyn_system_params);
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
	fn cancel(&mut self, _dyn_system_params: &mut [DynSystemParam], params: &SDT::Params, requester_node: Entity) {
		if let Some(waiting) = self.waiting.get_mut(params) {
			waiting.remove(&requester_node);
			if waiting.is_empty() {
				self.waiting.remove(params);
			}
		}

		let Some(stream) = self.streams.get_mut(params) else { return; };
		stream.usages.remove(&requester_node);
		let source_entity = stream.source_entity;
		if stream.live && stream.usages.is_empty() && !self.waiting.contains_key(params) {
			self.data_to_drop.entry(params.clone()).or_default().insert(source_entity);
			self.streams.remove(params);
		}
	}
}

#[derive(SystemParam)]
pub struct DataDirtier<'w, 's, SDT: StreamingDataType> {
	bile_state_tick: Res<'w, StateTick>,
	data_storages: Query<'w, 's, One<&'static mut dyn DataStorage<SDT>>>,
	data_consuming_nodes: DataConsumingNodes<'w, 's, SDT>,
}

impl<'w, 's, SDT: StreamingDataType> DataDirtier<'w, 's, SDT> {
	pub fn mark_dirty(&mut self, params: &SDT::Params, diff: Option<&SDT::Diff>) {
		let last_clean_tick = self.bile_state_tick.update();
		for mut data_storage in &mut self.data_storages {
			data_storage.dirty_data(&mut self.data_consuming_nodes, params, last_clean_tick, diff);
		}
	}
}

#[derive(SystemParam)]
pub struct DataPasser<'w, 's, SDT: StreamingDataType> {
	data_storages: Query<'w, 's, One<&'static mut dyn DataStorage<SDT>>>,
}

impl<'w, 's, SDT: StreamingDataType> DataPasser<'w, 's, SDT> {
	/// Producer calls this once, before any `push`.
	pub fn start(&mut self, source_entity: Entity, params: &SDT::Params, stream_id: StreamId, dirty_state: DirtyState) {
		for mut data_storage in &mut self.data_storages {
			data_storage.start(source_entity, params, stream_id, dirty_state);
		}
	}

	/// Producer calls this any number of times between `start` and `finish`.
	pub fn push(&mut self, source_entity: Entity, params: &SDT::Params, stream_id: StreamId, data: SDT::Data) {
		for mut data_storage in &mut self.data_storages {
			data_storage.push(source_entity, params, stream_id, &data);
		}
	}

	/// Producer calls this once to close out the stream.
	pub fn finish(&mut self, source_entity: Entity, params: &SDT::Params, stream_id: StreamId) {
		for mut data_storage in &mut self.data_storages {
			data_storage.finish(source_entity, params, stream_id);
		}
	}
}

impl<SDT: StreamingDataType> DataStorage<SDT> for DataColumn<SDT> {
	fn start(&mut self, source_entity: Entity, params: &SDT::Params, stream_id: StreamId, dirty_state: DirtyState) {
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
	fn push(&mut self, source_entity: Entity, params: &SDT::Params, stream_id: StreamId, data: &SDT::Data) {
		let Some(stream) = self.streams.get_mut(params) else { return; };
		if stream.stream_id != stream_id || !stream.live || stream.source_entity != source_entity { return; } // stale stream, ignore
		stream.data.push(data.clone());
		if stream.usages.is_empty() { return; }
		let usages = stream.usages.clone();
		self.events.push((params.clone(), StreamEvent::Push { entities: usages, data: data.clone() }));
	}
	fn finish(&mut self, source_entity: Entity, params: &SDT::Params, stream_id: StreamId) {
		let Some(stream) = self.streams.get_mut(params) else { return; };
		if stream.stream_id != stream_id || !stream.live || stream.source_entity != source_entity { return; } // stale stream, ignore
		stream.live = false;
		let entities = stream.usages.clone();
		if !entities.is_empty() {
			self.events.push((params.clone(), StreamEvent::Finish { entities }));
		}
	}
	fn dirty_data(&mut self, data_consuming_nodes: &mut DataConsumingNodes<SDT>, params: &SDT::Params, last_clean_tick: u64, diff: Option<&SDT::Diff>) {
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
	fn uses(&self, params: &SDT::Params, source_entity: Entity) -> bool {
		self.waiting.contains_key(params) || self.streams.get(params).is_some_and(|stream| stream.source_entity == source_entity)
	}
	fn drop_data(&mut self) -> HashMap<SDT::Params, HashSet<Entity>> { std::mem::take(&mut self.data_to_drop) }
}

pub fn pass_data<SDT: StreamingDataType>(
	bile_state_tick: Res<StateTick>,
	mut data_column: ResMut<DataColumn<SDT>>,
	mut data_consuming_nodes: DataConsumingNodes<SDT>,
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

pub fn drop_data<SDT: StreamingDataType>(
	mut data_storages: Query<One<&'static mut dyn DataStorage<SDT>>>,
	mut data_producing_nodes: Query<One<&'static mut dyn DataProducingNode<SDT>>>,
) {
	let mut data_to_drop: HashMap<SDT::Params, HashSet<Entity>> = HashMap::default();
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

fn setup_bevy_resources<SDT: StreamingDataType>(app: &mut App) {
	if app.world().contains_resource::<DataColumn<SDT>>() { return; }
	app
		.add_systems(BileDataSchedule, (
			pass_data::<SDT>.in_set(BileDataSystemSet::PassData),
			drop_data::<SDT>.in_set(BileDataSystemSet::DropData),
		))
		.init_resource::<DataColumn<SDT>>()
		.register_component_as::<dyn DataRequestingAPI<SDT>, DataColumn<SDT>>()
		.register_component_as::<dyn DataStorage<SDT>, DataColumn<SDT>>();
}

pub trait BileRegisterStreaming {
	fn register_producer_streaming<SDT, C>(&mut self) -> &mut Self
	where SDT: StreamingDataType, C: Component + DataProducingNode<SDT>;
	fn register_consumer_streaming<SDT, C>(&mut self) -> &mut Self
	where SDT: StreamingDataType, C: Component + DataConsumingNode<SDT>;
}

impl BileRegisterStreaming for App {
	fn register_producer_streaming<SDT, C>(&mut self) -> &mut Self
	where SDT: StreamingDataType, C: Component + DataProducingNode<SDT>,
	{
		self.register_component_as::<dyn DataProducingNode<SDT>, C>();
		setup_bevy_resources::<SDT>(self);
		self
	}

	fn register_consumer_streaming<SDT, C>(&mut self) -> &mut Self
	where SDT: StreamingDataType, C: Component + DataConsumingNode<SDT>,
	{
		self.register_component_as::<dyn DataConsumingNode<SDT>, C>();
		setup_bevy_resources::<SDT>(self);
		self
	}
}
