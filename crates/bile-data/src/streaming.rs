use std::{collections::{HashMap, HashSet, hash_map::Entry}, hash::Hash, marker::PhantomData, sync::atomic::{AtomicU64, Ordering}};

use bevy::{
	ecs::{schedule::IntoScheduleConfigs, system::{DynParamBuilder, DynSystemParam, ParamBuilder, ScheduleSystem, SystemParam}},
	prelude::*,
};
use bevy_trait_query::{One, RegisterExt, TraitQuery};

use crate::{default::{BileDataSchedule, BileDataSystemSet, DirtyState, StateTick}, dyn_system_params::{DynSystemParams, HasDynSystemParam, downcast}, gather};

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

pub type DataProducingNodes<'w, 's, N> = Query<'w, 's, (One<&'static mut N>, Entity)>;
pub type DataConsumingNodes<'w, 's, SDT> = Query<'w, 's, One<&'static mut dyn DataConsumingNode<SDT>>>;

// The nodes a storage gets its data from. Which of them are asked is all that the storages differ in.
pub trait SourceNodes<SDT: StreamingDataType>: TraitQuery {
	fn cost(nodes: &DataProducingNodes<Self>, request: &Request<SDT>) -> Option<f32>; // None if the request can not be fulfilled
	fn request(nodes: &mut DataProducingNodes<Self>, request: Request<SDT>, stream_id: StreamId) -> HashSet<Entity>; // the nodes that were asked
}

// The best node is asked.
impl<SDT: StreamingDataType> SourceNodes<SDT> for dyn DataProducingNode<SDT> {
	fn cost(nodes: &DataProducingNodes<Self>, request: &Request<SDT>) -> Option<f32> { best_node(nodes, request).map(|best| best.0) }
	fn request(nodes: &mut DataProducingNodes<Self>, request: Request<SDT>, stream_id: StreamId) -> HashSet<Entity> {
		let node_entity = best_node(nodes, &request).expect("Data request failed to find node to fulfill the request. This should never happen!").1;
		nodes.get_mut(node_entity).expect("This must exist as it was found in best_node").0.request(request, stream_id);
		HashSet::from([node_entity])
	}
}

// None if no node can fulfill the request
fn best_node<SDT: StreamingDataType>(data_producing_node: &DataProducingNodes<dyn DataProducingNode<SDT>>, request: &Request<SDT>) -> Option<(f32, Entity)> {
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

// What was passed for some params. It is kept, also when it is dirty, for as long as something uses it or waits for it.
pub struct DataInfo<SDT: StreamingDataType> {
	pub dirty_state: DirtyState,
	pub data: Vec<SDT::Data>,
	pub usages: HashSet<Entity>,
	pub sources: HashSet<Entity>,
	diffs: Option<Vec<SDT::Diff>>, // every dirty since the data was made. None if one had no diff
	stream: Option<Stream>, // the stream the data comes from. None once every source finished
}

impl<SDT: StreamingDataType> DataInfo<SDT> {
	pub fn diffs(&self) -> Option<&[SDT::Diff]> { self.diffs.as_deref() }
}

// One passing of the data: from the nodes to the storage and from the storage to the consumers that asked for it.
// The data stays when the stream is over, and a later stream for the same params replaces the data in place.
struct Stream {
	id: StreamId,
	nodes: HashSet<Entity>, // the nodes that did not finish yet
	consumers: HashSet<Entity>,
}

enum StreamEvent<SDT: StreamingDataType> {
	Start { entities: HashSet<Entity> },
	Push { entities: HashSet<Entity>, data: SDT::Data },
	Finish { entities: HashSet<Entity> },
}

// The storage of what the nodes `N` stream.
#[derive(Resource)]
pub struct Column<SDT: StreamingDataType, N: ?Sized + SourceNodes<SDT>> {
	data: HashMap<SDT::Params, DataInfo<SDT>>,
	requested: HashMap<SDT::Params, Stream>, // the stream that was asked for the waiting, until a node starts it
	data_to_drop: HashMap<SDT::Params, HashSet<Entity>>, // the nodes of data and streams that are gone. They are dropped unless something still uses them
	waiting: HashMap<SDT::Params, HashMap<Entity, DirtyState>>,
	// Ordered (unlike the other modules' HashMap-based `finished`) because
	// start/push.../finish must be delivered to consumers in that order.
	events: Vec<(SDT::Params, StreamEvent<SDT>)>,
	nodes: PhantomData<fn(&N)>,
}

pub type DataColumn<SDT> = Column<SDT, dyn DataProducingNode<SDT>>;

impl<SDT: StreamingDataType, N: ?Sized + SourceNodes<SDT>> Default for Column<SDT, N> {
	fn default() -> Self {
		Self {
			data: HashMap::default(),
			requested: HashMap::default(),
			data_to_drop: HashMap::default(),
			waiting: HashMap::default(),
			events: Vec::default(),
			nodes: PhantomData,
		}
	}
}

impl<SDT: StreamingDataType, N: ?Sized + SourceNodes<SDT>> Column<SDT, N> {
	pub fn get(&self, params: &SDT::Params) -> Option<&DataInfo<SDT>> { self.data.get(params) }
	// Nothing is kept or asked for when the params have no user and no waiter.
	fn release_unused(&mut self, params: &SDT::Params) {
		if self.waiting.contains_key(params) { return; }
		if let Some(stream) = self.requested.remove(params) {
			self.data_to_drop.entry(params.clone()).or_default().extend(stream.nodes);
		}
		if let Entry::Occupied(info) = self.data.entry(params.clone()) && info.get().usages.is_empty() {
			self.data_to_drop.entry(params.clone()).or_default().extend(info.remove().sources);
		}
	}
}

impl<SDT: StreamingDataType, N: ?Sized + SourceNodes<SDT>> DataRequestingAPI<SDT> for Column<SDT, N> {
	fn dyn_system_param(&self) -> DynParamBuilder<'static> { DynParamBuilder::new::<DataProducingNodes<N>>(ParamBuilder) }
	fn cost(&self, dyn_system_params: &mut [DynSystemParam], request: &Request<SDT>) -> Option<f32> {
		N::cost(&downcast(dyn_system_params), request)
	}
	fn request(&mut self, dyn_system_params: &mut [DynSystemParam], request: Request<SDT>, requester_node: Entity) {
		if let Some(info) = self.data.get_mut(&request.params) && info.dirty_state.satisfies(&request.allowed_tick) {
			info.usages.insert(requester_node);
			let entities = HashSet::from([requester_node]);
			self.events.push((request.params.clone(), StreamEvent::Start { entities: entities.clone() }));
			self.events.extend(info.data.iter().map(|data| (request.params.clone(), StreamEvent::Push { entities: entities.clone(), data: data.clone() })));
			match &mut info.stream {
				Some(stream) => { stream.consumers.insert(requester_node); } // the rest comes with the stream
				None => self.events.push((request.params, StreamEvent::Finish { entities })),
			}
			return;
		}
		let already_requested = self.waiting.get(&request.params).is_some_and(|waiting| waiting.values().any(|required| required.satisfies(&request.allowed_tick)));
		// update requester
		self.waiting.entry(request.params.clone()).or_default().insert(requester_node, request.allowed_tick);
		if already_requested { return; }
		// issue a fresh stream id and ask the nodes for the stream
		let (params, id) = (request.params.clone(), StreamId::new());
		let nodes = N::request(&mut downcast(dyn_system_params), request, id);
		if let Some(replaced) = self.requested.insert(params.clone(), Stream { id, nodes, consumers: HashSet::default() }) {
			self.data_to_drop.entry(params).or_default().extend(replaced.nodes);
		}
	}
	fn cancel(&mut self, _dyn_system_params: &mut [DynSystemParam], params: &SDT::Params, requester_node: Entity) {
		if let Some(waiting) = self.waiting.get_mut(params) {
			waiting.remove(&requester_node);
			if waiting.is_empty() {
				self.waiting.remove(params);
			}
		}
		if let Some(info) = self.data.get_mut(params) {
			info.usages.remove(&requester_node);
			if let Some(stream) = &mut info.stream {
				stream.consumers.remove(&requester_node);
			}
		}
		self.release_unused(params);
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

impl<SDT: StreamingDataType, N: ?Sized + SourceNodes<SDT>> DataStorage<SDT> for Column<SDT, N> {
	fn start(&mut self, _source_entity: Entity, params: &SDT::Params, stream_id: StreamId, dirty_state: DirtyState) {
		if let Some(info) = self.data.get_mut(params) && info.stream.as_ref().is_some_and(|stream| stream.id == stream_id) {
			// another node joins the stream, the data is only as fresh as its least fresh part
			if info.dirty_state.is_fresher_than(&dirty_state) {
				info.dirty_state = dirty_state;
				info.diffs = None;
			}
			return;
		}

		let Entry::Occupied(requested) = self.requested.entry(params.clone()) else { return; };
		if requested.get().id != stream_id { return; } // the stream of another storage, or one that nothing waits for anymore
		let stream = requested.remove();
		if let Some(existing) = self.data.get(params) && existing.dirty_state.is_fresher_than(&dirty_state) {
			// the data that is already here is fresher; reject outright
			self.data_to_drop.entry(params.clone()).or_default().extend(stream.nodes);
			return;
		}

		let waiting = self.waiting.remove(params).expect("A stream is only requested for as long as something waits for it");
		let mut consumers = HashSet::default();
		let mut still_waiting = HashMap::default();
		for (entity, required) in waiting {
			if dirty_state.satisfies(&required) {
				consumers.insert(entity);
			} else {
				still_waiting.insert(entity, required);
			}
		}
		if !still_waiting.is_empty() {
			self.waiting.insert(params.clone(), still_waiting);
		}
		if !consumers.is_empty() {
			self.events.push((params.clone(), StreamEvent::Start { entities: consumers.clone() }));
		}

		let diffs = matches!(dirty_state, DirtyState::UpToDate).then(Vec::new);
		let (sources, stream) = (stream.nodes.clone(), Some(Stream { consumers: consumers.clone(), ..stream }));
		match self.data.entry(params.clone()) {
			Entry::Occupied(mut entry) => {
				// The data is replaced in place. Its users stay its users, but only the ones that asked are passed the new stream.
				let info = entry.get_mut();
				let old_sources = std::mem::replace(&mut info.sources, sources);
				self.data_to_drop.entry(params.clone()).or_default().extend(old_sources);
				info.usages.extend(consumers);
				info.data.clear();
				(info.dirty_state, info.diffs, info.stream) = (dirty_state, diffs, stream);
			}
			Entry::Vacant(entry) => {
				entry.insert(DataInfo { dirty_state, data: Vec::new(), usages: consumers, sources, diffs, stream });
			}
		}
	}
	fn push(&mut self, source_entity: Entity, params: &SDT::Params, stream_id: StreamId, data: &SDT::Data) {
		let Some(info) = self.data.get_mut(params) else { return; };
		let Some(stream) = info.stream.as_ref().filter(|stream| stream.id == stream_id && stream.nodes.contains(&source_entity)) else { return; }; // stale stream, ignore
		info.data.push(data.clone());
		if stream.consumers.is_empty() { return; }
		self.events.push((params.clone(), StreamEvent::Push { entities: stream.consumers.clone(), data: data.clone() }));
	}
	fn finish(&mut self, source_entity: Entity, params: &SDT::Params, stream_id: StreamId) {
		let Some(info) = self.data.get_mut(params) else { return; };
		let Some(stream) = info.stream.as_mut().filter(|stream| stream.id == stream_id) else { return; }; // stale stream, ignore
		if !stream.nodes.remove(&source_entity) || !stream.nodes.is_empty() { return; }
		// every node finished, so the stream is over
		let entities = std::mem::take(&mut stream.consumers);
		info.stream = None;
		if !entities.is_empty() {
			self.events.push((params.clone(), StreamEvent::Finish { entities }));
		}
	}
	fn dirty_data(&mut self, data_consuming_nodes: &mut DataConsumingNodes<SDT>, params: &SDT::Params, last_clean_tick: u64, diff: Option<&SDT::Diff>) {
		let Some(info) = self.data.get_mut(params) else { return; };
		info.dirty_state = DirtyState::Dirty { tick: last_clean_tick };
		match (&mut info.diffs, diff) {
			(Some(diffs), Some(diff)) => diffs.push(diff.clone()),
			_ => info.diffs = None, // There may never be a dirty inbetween 2 diffs
		}
		info.usages.retain(|&usage| {
			let Ok(mut node) = data_consuming_nodes.get_mut(usage) else {
				bevy::log::error!(name: "Missing DataConsumingNode", "Usage was not removed for non existent DataConsumingNode {}.", usage);
				return false;
			};
			node.dirty_data(params, last_clean_tick, diff);
			true
		})
	}
	// The data is only here while something uses it or waits for it, and a stream is only requested while something waits for it.
	fn uses(&self, params: &SDT::Params, source_entity: Entity) -> bool {
		self.data.get(params).is_some_and(|info| info.sources.contains(&source_entity)) || self.requested.get(params).is_some_and(|stream| stream.nodes.contains(&source_entity))
	}
	fn drop_data(&mut self) -> HashMap<SDT::Params, HashSet<Entity>> { std::mem::take(&mut self.data_to_drop) }
}

// The body of the pass_data system of a storage.
pub(crate) fn pass_events<SDT: StreamingDataType, N: ?Sized + SourceNodes<SDT>>(
	bile_state_tick: Res<StateTick>,
	mut data_column: ResMut<Column<SDT, N>>,
	mut data_consuming_nodes: DataConsumingNodes<SDT>,
) {
	if data_column.events.is_empty() { return; }

	for (params, event) in std::mem::take(&mut data_column.events) {
		match event {
			StreamEvent::Start { entities } => {
				let tick = match data_column.data.get(&params).map(|info| info.dirty_state) {
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

pub fn pass_data<SDT: StreamingDataType>(bile_state_tick: Res<StateTick>, data_column: ResMut<DataColumn<SDT>>, data_consuming_nodes: DataConsumingNodes<SDT>) {
	pass_events(bile_state_tick, data_column, data_consuming_nodes);
}

// For the nodes of every storage of the data type, the ones that are asked alone and the ones of a gather.
pub fn drop_data<SDT: StreamingDataType>(
	mut data_storages: Query<One<&'static mut dyn DataStorage<SDT>>>,
	mut data_producing_nodes: ParamSet<(Query<One<&'static mut dyn DataProducingNode<SDT>>>, Query<One<&'static mut dyn gather::DataProducingNode<SDT>>>)>,
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
			if let Ok(mut node) = data_producing_nodes.p0().get_mut(source_entity) {
				node.drop(&params);
			}
			if let Ok(mut node) = data_producing_nodes.p1().get_mut(source_entity) {
				node.drop(&params);
			}
		}
	}
}

pub(crate) fn setup_bevy_resources<SDT: StreamingDataType, N: ?Sized + SourceNodes<SDT>, M>(app: &mut App, pass_data: impl IntoScheduleConfigs<ScheduleSystem, M>) {
	if app.world().contains_resource::<Column<SDT, N>>() { return; }
	if !app.world().contains_resource::<DataColumn<SDT>>() && !app.world().contains_resource::<gather::DataColumn<SDT>>() {
		app.add_systems(BileDataSchedule, drop_data::<SDT>.in_set(BileDataSystemSet::DropData));
	}
	app
		.add_systems(BileDataSchedule, pass_data.in_set(BileDataSystemSet::PassData))
		.init_resource::<Column<SDT, N>>()
		.register_component_as::<dyn DataRequestingAPI<SDT>, Column<SDT, N>>()
		.register_component_as::<dyn DataStorage<SDT>, Column<SDT, N>>();
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
		setup_bevy_resources::<SDT, dyn DataProducingNode<SDT>, _>(self, pass_data::<SDT>);
		self
	}

	fn register_consumer_streaming<SDT, C>(&mut self) -> &mut Self
	where SDT: StreamingDataType, C: Component + DataConsumingNode<SDT>,
	{
		self.register_component_as::<dyn DataConsumingNode<SDT>, C>();
		setup_bevy_resources::<SDT, dyn DataProducingNode<SDT>, _>(self, pass_data::<SDT>);
		self
	}
}
