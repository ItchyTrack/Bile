use std::{
	any::type_name,
	collections::{HashMap, HashSet},
	marker::PhantomData,
	sync::atomic::{AtomicU64, Ordering},
};

use bevy::{
	ecs::system::{StaticSystemParam, SystemParam, SystemParamItem},
	prelude::*,
};
use bevy_trait_query::{One, RegisterExt};

use crate::{
	dyn_system_params::{DynParam, DynSystemParams, RegisterDynParam},
	parts::{BileSetDataParts, DataParts, Part, SetupParts},
	setup::{BileDataSchedule, BileDataSystemSet, DataType, DirtyState, Request, StateTick},
};

// ============================ Nodes ============================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StreamId(u64);

impl StreamId {
	// Every request gets a new id, so that a stale stream can be told apart from the newest one.
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
pub trait DataProducingNode<DT: DataType> {
	fn cost(&self, request: &Request<DT>) -> Option<f32>; // None if the node can not fulfill the request
	fn request(&mut self, request: Request<DT>, stream_id: StreamId);
	fn drop(&mut self, params: &DT::Params);
}

pub type DataProducingNodes<'w, 's, DT> = Query<'w, 's, (One<&'static mut dyn DataProducingNode<DT>>, Entity)>;
pub type DataConsumingNodes<'w, 's, DT> = Query<'w, 's, One<&'static mut dyn DataConsumingNode<DT>>>;

// None if no node can fulfill the request
pub fn best_node<DT: DataType>(data_producing_nodes: &DataProducingNodes<DT>, request: &Request<DT>) -> Option<(f32, Entity)> {
	data_producing_nodes.iter().filter_map(|(node, entity)| Some((node.cost(request)?, entity))).min_by(|a, b| a.0.total_cmp(&b.0))
}

// ============================ Entry points ============================

#[derive(SystemParam)]
pub struct DataRequester<'w, 's, DT: DataType> {
	parts: Query<'w, 's, One<&'static mut dyn StreamParts<DT>>>,
	system_params: DynSystemParams<'w, 's, Requesting<DT>>,
}

impl<DT: DataType> DataRequester<'_, '_, DT> {
	pub fn request(&mut self, request: Request<DT>, requester_node: Entity) {
		let Ok(mut parts) = self.parts.single_mut() else { panic!("{} has no data parts, a node of it has to be registered.", type_name::<DT>()) };
		parts.request(&mut self.system_params, request, requester_node);
	}
	pub fn cancel(&mut self, params: &DT::Params, requester_node: Entity) {
		if let Ok(mut parts) = self.parts.single_mut() {
			parts.cancel(params, requester_node);
		}
	}
}

#[derive(SystemParam)]
pub struct DataPasser<'w, 's, DT: DataType> {
	parts: Query<'w, 's, One<&'static mut dyn StreamParts<DT>>>,
}

impl<DT: DataType> DataPasser<'_, '_, DT> {
	/// Producer calls this once, before any `push`.
	pub fn start(&mut self, source_entity: Entity, params: &DT::Params, stream_id: StreamId, dirty_state: DirtyState) {
		if let Ok(mut parts) = self.parts.single_mut() {
			parts.start(source_entity, params, stream_id, dirty_state);
		}
	}
	/// Producer calls this any number of times between `start` and `finish`.
	pub fn push(&mut self, source_entity: Entity, params: &DT::Params, stream_id: StreamId, data: DT::Data) {
		if let Ok(mut parts) = self.parts.single_mut() {
			parts.push(source_entity, params, stream_id, data);
		}
	}
	/// Producer calls this once to close out the stream.
	pub fn finish(&mut self, source_entity: Entity, params: &DT::Params, stream_id: StreamId) {
		if let Ok(mut parts) = self.parts.single_mut() {
			parts.finish(source_entity, params, stream_id);
		}
	}
}

#[derive(SystemParam)]
pub struct DataDirtier<'w, 's, DT: DataType> {
	bile_state_tick: Res<'w, StateTick>,
	parts: Query<'w, 's, One<&'static mut dyn StreamParts<DT>>>,
	system_params: DynSystemParams<'w, 's, Dirtying<DT>>,
}

impl<DT: DataType> DataDirtier<'_, '_, DT> {
	pub fn mark_dirty(&mut self, params: &DT::Params, diff: Option<&DT::Diff>) {
		let last_clean_tick = self.bile_state_tick.update();
		if let Ok(mut parts) = self.parts.single_mut() {
			parts.mark_dirty(&mut self.system_params, params, last_clean_tick, diff);
		}
	}
}

/// What the entry points need the params of the parts for.
pub struct Requesting<DT>(PhantomData<fn() -> DT>);
pub struct Dirtying<DT>(PhantomData<fn() -> DT>);

// The entry points reach the parts of a data type through this, they do not know their types.
#[bevy_trait_query::queryable]
pub trait StreamParts<DT: DataType> {
	fn request(&mut self, system_params: &mut DynSystemParams<Requesting<DT>>, request: Request<DT>, requester_node: Entity);
	fn cancel(&mut self, params: &DT::Params, requester_node: Entity);
	fn start(&mut self, source_entity: Entity, params: &DT::Params, stream_id: StreamId, dirty_state: DirtyState);
	fn push(&mut self, source_entity: Entity, params: &DT::Params, stream_id: StreamId, data: DT::Data);
	fn finish(&mut self, source_entity: Entity, params: &DT::Params, stream_id: StreamId);
	fn mark_dirty(&mut self, system_params: &mut DynSystemParams<Dirtying<DT>>, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>);
}

// ============================ Parts ============================

/// Streams the data to the nodes that request it, from the storage `S` or else by having the fulfiller `F` get it.
pub trait Requester<DT: DataType, S, F: Fulfiller<DT, S>>: Part {
	type NodesParam: SystemParam + 'static; // to reach the consuming nodes

	fn request(&mut self, storage: &mut S, fulfiller: &mut F, fulfiller_param: &mut SystemParamItem<'_, '_, F::Param>, request: Request<DT>, requester_node: Entity);
	fn cancel(&mut self, storage: &mut S, params: &DT::Params, requester_node: Entity);
	/// The fulfiller started a new stream in the storage.
	fn started(&mut self, storage: &mut S, params: &DT::Params);
	/// The fulfiller pushed data to the stream.
	fn pushed(&mut self, storage: &mut S, params: &DT::Params, data: &DT::Data);
	/// The stream is complete.
	fn finished(&mut self, storage: &mut S, params: &DT::Params);
	fn mark_dirty(&mut self, storage: &mut S, nodes: &mut SystemParamItem<'_, '_, Self::NodesParam>, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>);
	/// Passes what was streamed to the nodes, in order.
	fn pass_data(&mut self, storage: &mut S, nodes: &mut SystemParamItem<'_, '_, Self::NodesParam>, current_tick: u64);
}

/// Gets the streams into the storage `S` from the producing nodes.
pub trait Fulfiller<DT: DataType, S>: Part {
	type Param: SystemParam + 'static; // to reach the producing nodes

	/// Has the data streamed, unless it already is.
	fn fulfill(&mut self, storage: &mut S, param: &mut SystemParamItem<'_, '_, Self::Param>, request: Request<DT>);
	/// Returns true if it started a new stream.
	fn start(&mut self, storage: &mut S, source_entity: Entity, params: &DT::Params, stream_id: StreamId, dirty_state: DirtyState) -> bool;
	/// Returns true if the data was pushed to the stream.
	fn push(&mut self, storage: &mut S, source_entity: Entity, params: &DT::Params, stream_id: StreamId, data: &DT::Data) -> bool;
	/// Returns true if the stream is complete.
	fn finish(&mut self, storage: &mut S, source_entity: Entity, params: &DT::Params, stream_id: StreamId) -> bool;
	fn mark_dirty(&mut self, storage: &mut S, params: &DT::Params, last_clean_tick: u64);
	/// Drops the nodes the stored streams no longer come from.
	fn drop_data(&mut self, storage: &mut S, param: &mut SystemParamItem<'_, '_, Self::Param>);
}

impl<DT, R, S, F> StreamParts<DT> for DataParts<DT, R, S, F>
where DT: DataType, R: Requester<DT, S, F>, S: Part, F: Fulfiller<DT, S>,
{
	fn request(&mut self, system_params: &mut DynSystemParams<Requesting<DT>>, request: Request<DT>, requester_node: Entity) {
		self.requester.request(&mut self.storage, &mut self.fulfiller, &mut system_params.get::<Self>(), request, requester_node);
	}
	fn cancel(&mut self, params: &DT::Params, requester_node: Entity) { self.requester.cancel(&mut self.storage, params, requester_node); }
	fn start(&mut self, source_entity: Entity, params: &DT::Params, stream_id: StreamId, dirty_state: DirtyState) {
		if self.fulfiller.start(&mut self.storage, source_entity, params, stream_id, dirty_state) {
			self.requester.started(&mut self.storage, params);
		}
	}
	fn push(&mut self, source_entity: Entity, params: &DT::Params, stream_id: StreamId, data: DT::Data) {
		if self.fulfiller.push(&mut self.storage, source_entity, params, stream_id, &data) {
			self.requester.pushed(&mut self.storage, params, &data);
		}
	}
	fn finish(&mut self, source_entity: Entity, params: &DT::Params, stream_id: StreamId) {
		if self.fulfiller.finish(&mut self.storage, source_entity, params, stream_id) {
			self.requester.finished(&mut self.storage, params);
		}
	}
	fn mark_dirty(&mut self, system_params: &mut DynSystemParams<Dirtying<DT>>, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>) {
		self.fulfiller.mark_dirty(&mut self.storage, params, last_clean_tick);
		self.requester.mark_dirty(&mut self.storage, &mut system_params.get::<Self>(), params, last_clean_tick, diff);
	}
}

impl<DT, R, S, F> DynParam<Requesting<DT>> for DataParts<DT, R, S, F>
where DT: DataType, R: Requester<DT, S, F>, S: Part, F: Fulfiller<DT, S>,
{
	type Param = F::Param;
}

impl<DT, R, S, F> DynParam<Dirtying<DT>> for DataParts<DT, R, S, F>
where DT: DataType, R: Requester<DT, S, F>, S: Part, F: Fulfiller<DT, S>,
{
	type Param = R::NodesParam;
}

fn setup<DT, R, S, F>(app: &mut App)
where DT: DataType, R: Requester<DT, S, F>, S: Part, F: Fulfiller<DT, S>,
{
	app.register_component_as::<dyn StreamParts<DT>, DataParts<DT, R, S, F>>()
		.register_dyn_param::<Requesting<DT>, DataParts<DT, R, S, F>>()
		.register_dyn_param::<Dirtying<DT>, DataParts<DT, R, S, F>>()
		.add_systems(BileDataSchedule, (
			drop_data::<DT, R, S, F>.in_set(BileDataSystemSet::DropData),
			pass_data::<DT, R, S, F>.in_set(BileDataSystemSet::PassData),
		));
}

fn drop_data<DT, R, S, F>(mut parts: Query<&mut DataParts<DT, R, S, F>>, mut param: StaticSystemParam<F::Param>)
where DT: DataType, R: Requester<DT, S, F>, S: Part, F: Fulfiller<DT, S>,
{
	for mut parts in &mut parts {
		let DataParts { storage, fulfiller, .. } = &mut *parts;
		fulfiller.drop_data(storage, &mut param);
	}
}

fn pass_data<DT, R, S, F>(bile_state_tick: Res<StateTick>, mut parts: Query<&mut DataParts<DT, R, S, F>>, mut nodes: StaticSystemParam<R::NodesParam>)
where DT: DataType, R: Requester<DT, S, F>, S: Part, F: Fulfiller<DT, S>,
{
	for mut parts in &mut parts {
		let DataParts { requester, storage, .. } = &mut *parts;
		requester.pass_data(storage, &mut nodes, bile_state_tick.current_tick());
	}
}

/// The stream that is currently accepted for a given `params`. After it is finished it is kept only while something
/// still uses it or waits for it, so that late requesters get the replay and a staler stream is rejected.
pub struct Stream<DT: DataType> {
	pub stream_id: StreamId,
	pub dirty_state: DirtyState,
	pub data: Vec<DT::Data>,
	pub finished: bool,
}

impl<DT: DataType> Stream<DT> {
	pub fn new(stream_id: StreamId, dirty_state: DirtyState) -> Self { Self { stream_id, dirty_state, data: Vec::new(), finished: false } }
}

// ============================ Stream Requester ============================

/// What the stream requester needs from a storage.
pub trait StreamRequesterStorage<DT: DataType>: Part {
	fn stream(&self, params: &DT::Params) -> Option<&Stream<DT>>;
	fn requests(&mut self) -> &mut StreamRequests<DT>;
	/// No node uses or waits for the stream anymore. The cancel of its last user releases it.
	fn release(&mut self, params: &DT::Params);
}

/// What the stream requester keeps in the storage: the nodes that requested the streams.
pub struct StreamRequests<DT: DataType> {
	waiting: HashMap<DT::Params, HashMap<Entity, DirtyState>>, // and how fresh the stream they wait for has to be
	usages: HashMap<DT::Params, HashSet<Entity>>,               // they get the stream that is accepted
	// Ordered (unlike the simple requester's `finished`) because start/push.../finish must be delivered in that order.
	events: Vec<(DT::Params, StreamEvent<DT>)>,
}

enum StreamEvent<DT: DataType> {
	Start { entities: HashSet<Entity> },
	Push { entities: HashSet<Entity>, data: DT::Data },
	Finish { entities: HashSet<Entity> },
}

impl<DT: DataType> Default for StreamRequests<DT> {
	fn default() -> Self { Self { waiting: HashMap::default(), usages: HashMap::default(), events: Vec::default() } }
}

impl<DT: DataType> StreamRequests<DT> {
	pub fn is_waiting(&self, params: &DT::Params) -> bool { self.waiting.contains_key(params) }
	/// If a stream that satisfies `allowed_tick` is already on its way.
	pub fn is_waiting_for(&self, params: &DT::Params, allowed_tick: &DirtyState) -> bool {
		self.waiting.get(params).is_some_and(|waiting| waiting.values().any(|required| required.satisfies(allowed_tick)))
	}
	pub fn is_requested(&self, params: &DT::Params) -> bool { self.waiting.contains_key(params) || self.usages.contains_key(params) }
	fn forget(&mut self, params: &DT::Params, requester_node: Entity) {
		if self.waiting.get_mut(params).is_some_and(|waiting| { waiting.remove(&requester_node); waiting.is_empty() }) {
			self.waiting.remove(params);
		}
		if self.usages.get_mut(params).is_some_and(|usages| { usages.remove(&requester_node); usages.is_empty() }) {
			self.usages.remove(params);
		}
	}
	fn send(&mut self, params: &DT::Params, event: impl FnOnce(HashSet<Entity>) -> StreamEvent<DT>) {
		if let Some(usages) = self.usages.get(params) {
			self.events.push((params.clone(), event(usages.clone())));
		}
	}
}

/// Streams the data to the nodes that request it. A node that requests a stream that is already accepted gets what
/// was already streamed and then the rest.
#[derive(Default)]
pub struct StreamRequester;

impl<DT, S, F> Requester<DT, S, F> for StreamRequester
where DT: DataType, S: StreamRequesterStorage<DT>, F: Fulfiller<DT, S>,
{
	type NodesParam = DataConsumingNodes<'static, 'static, DT>;

	fn request(&mut self, storage: &mut S, fulfiller: &mut F, fulfiller_param: &mut SystemParamItem<F::Param>, request: Request<DT>, requester_node: Entity) {
		if let Some(stream) = storage.stream(&request.params).filter(|stream| stream.dirty_state.satisfies(&request.allowed_tick)) {
			let entities = HashSet::from([requester_node]);
			let mut replay = vec![StreamEvent::Start { entities: entities.clone() }];
			replay.extend(stream.data.iter().map(|data| StreamEvent::Push { entities: entities.clone(), data: data.clone() }));
			if stream.finished {
				replay.push(StreamEvent::Finish { entities });
			}
			let requests = storage.requests();
			requests.usages.entry(request.params.clone()).or_default().insert(requester_node);
			requests.events.extend(replay.into_iter().map(|event| (request.params.clone(), event)));
			return;
		}
		let (params, allowed_tick) = (request.params.clone(), request.allowed_tick);
		fulfiller.fulfill(storage, fulfiller_param, request);
		storage.requests().waiting.entry(params).or_default().insert(requester_node, allowed_tick);
	}
	fn cancel(&mut self, storage: &mut S, params: &DT::Params, requester_node: Entity) {
		let requests = storage.requests();
		requests.forget(params, requester_node);
		if !requests.is_requested(params) {
			storage.release(params);
		}
	}
	fn started(&mut self, storage: &mut S, params: &DT::Params) {
		let Some(dirty_state) = storage.stream(params).map(|stream| stream.dirty_state) else { return; };
		let requests = storage.requests();
		let waiting = requests.waiting.remove(params).unwrap_or_default();
		let (usages, still_waiting): (HashMap<_, _>, HashMap<_, _>) = waiting.into_iter().partition(|(_, required)| dirty_state.satisfies(required));
		// the users of a replaced stream do not get the new one
		requests.usages.remove(params);
		if !still_waiting.is_empty() {
			requests.waiting.insert(params.clone(), still_waiting);
		}
		if !usages.is_empty() {
			requests.usages.insert(params.clone(), usages.into_keys().collect());
			requests.send(params, |entities| StreamEvent::Start { entities });
		}
	}
	fn pushed(&mut self, storage: &mut S, params: &DT::Params, data: &DT::Data) {
		storage.requests().send(params, |entities| StreamEvent::Push { entities, data: data.clone() });
	}
	fn finished(&mut self, storage: &mut S, params: &DT::Params) { storage.requests().send(params, |entities| StreamEvent::Finish { entities }); }
	fn mark_dirty(&mut self, storage: &mut S, nodes: &mut DataConsumingNodes<DT>, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>) {
		let Some(usages) = storage.requests().usages.get_mut(params) else { return; };
		usages.retain(|&usage| {
			let Ok(mut node) = nodes.get_mut(usage) else {
				bevy::log::error!(name: "Missing DataConsumingNode", "Usage was not removed for non existent DataConsumingNode {}.", usage);
				return false;
			};
			node.dirty_data(params, last_clean_tick, diff);
			true
		});
		if usages.is_empty() {
			storage.requests().usages.remove(params);
		}
	}
	fn pass_data(&mut self, storage: &mut S, nodes: &mut DataConsumingNodes<DT>, current_tick: u64) {
		if storage.requests().events.is_empty() { return; }
		for (params, event) in std::mem::take(&mut storage.requests().events) {
			match event {
				StreamEvent::Start { entities } => {
					let tick = match storage.stream(&params).map(|stream| stream.dirty_state) {
						Some(DirtyState::Dirty { tick }) => tick,
						_ => current_tick,
					};
					for entity in entities {
						let Ok(mut node) = nodes.get_mut(entity) else { continue; };
						node.start_data(&params, tick);
					}
				}
				StreamEvent::Push { entities, data } => {
					for entity in entities {
						let Ok(mut node) = nodes.get_mut(entity) else { continue; };
						node.push_data(&params, data.clone());
					}
				}
				StreamEvent::Finish { entities } => {
					for entity in entities {
						let Ok(mut node) = nodes.get_mut(entity) else { continue; };
						node.finish_data(&params);
					}
				}
			}
		}
	}
}

impl<DT, S, F> SetupParts<DT, S, F> for StreamRequester
where DT: DataType, S: StreamRequesterStorage<DT>, F: Fulfiller<DT, S>,
{
	fn setup(app: &mut App) { setup::<DT, Self, S, F>(app); }
}

// ============================ Stream Fulfiller ============================

/// What the stream fulfiller needs from a storage.
pub trait StreamFulfillerStorage<DT: DataType>: Part {
	fn stream(&self, params: &DT::Params) -> Option<&Stream<DT>>;
	fn stream_mut(&mut self, params: &DT::Params) -> Option<&mut Stream<DT>>;
	/// Replaces the stream that was accepted, live or not.
	fn start(&mut self, params: &DT::Params, stream: Stream<DT>);
	fn sources(&mut self) -> &mut StreamSources<DT>;
	/// Something waits for the stream, so it is accepted when it starts.
	fn wanted(&self, params: &DT::Params) -> bool;
	/// A stream that satisfies `allowed_tick` is already on its way.
	fn is_requested(&self, params: &DT::Params, allowed_tick: &DirtyState) -> bool;
}

/// What the stream fulfiller keeps in the storage: the nodes the streams come from.
pub struct StreamSources<DT: DataType> {
	requested: HashMap<DT::Params, StreamId>, // the newest stream a node was asked for, until it starts
	sources: HashMap<DT::Params, Entity>,     // the node of the accepted stream
	data_to_drop: HashMap<DT::Params, HashSet<Entity>>,
}

impl<DT: DataType> Default for StreamSources<DT> {
	fn default() -> Self { Self { requested: HashMap::default(), sources: HashMap::default(), data_to_drop: HashMap::default() } }
}

impl<DT: DataType> StreamSources<DT> {
	/// The stream is no longer stored, so the node it came from is dropped.
	pub fn release(&mut self, params: &DT::Params) {
		if let Some(source_entity) = self.sources.remove(params) {
			self.drop_source(params, source_entity);
		}
	}
	fn drop_source(&mut self, params: &DT::Params, source_entity: Entity) { self.data_to_drop.entry(params.clone()).or_default().insert(source_entity); }
	fn is_source(&self, params: &DT::Params, source_entity: Entity) -> bool { self.sources.get(params) == Some(&source_entity) }
}

/// Has the cheapest node stream the data.
#[derive(Default)]
pub struct StreamFulfiller;

impl StreamFulfiller {
	fn is_live<DT: DataType, S: StreamFulfillerStorage<DT>>(storage: &mut S, source_entity: Entity, params: &DT::Params, stream_id: StreamId) -> bool {
		storage.sources().is_source(params, source_entity) && storage.stream(params).is_some_and(|stream| stream.stream_id == stream_id && !stream.finished)
	}
}

impl<DT: DataType, S: StreamFulfillerStorage<DT>> Fulfiller<DT, S> for StreamFulfiller {
	type Param = DataProducingNodes<'static, 'static, DT>;

	fn fulfill(&mut self, storage: &mut S, nodes: &mut DataProducingNodes<DT>, request: Request<DT>) {
		if storage.is_requested(&request.params, &request.allowed_tick) { return; }
		let node_entity = best_node(nodes, &request).expect("Data request failed to find node to fulfill the request. This should never happen!").1;
		let sources = storage.sources();
		if let Some(dropped) = sources.data_to_drop.get_mut(&request.params) {
			dropped.remove(&node_entity);
		}
		let stream_id = StreamId::new();
		sources.requested.insert(request.params.clone(), stream_id);
		nodes.get_mut(node_entity).expect("This must exist as it was found in best_node").0.request(request, stream_id);
	}
	fn start(&mut self, storage: &mut S, source_entity: Entity, params: &DT::Params, stream_id: StreamId, dirty_state: DirtyState) -> bool {
		if storage.sources().requested.get(params) != Some(&stream_id) { return false; } // not the newest stream
		storage.sources().requested.remove(params);
		let existing = storage.stream(params).map(|stream| (stream.dirty_state, stream.finished));
		// something fresher is already active or already finished here, or no one wants this anymore
		if existing.is_some_and(|(existing, _)| existing.is_fresher_than(&dirty_state)) || !storage.wanted(params) {
			storage.sources().drop_source(params, source_entity);
			return false;
		}
		// supersede whatever was there, live or not; if it was still live, tell it to stop too
		if let Some((_, finished)) = existing && let Some(old_source) = storage.sources().sources.get(params).copied() && (!finished || old_source != source_entity) {
			storage.sources().drop_source(params, old_source);
		}
		storage.start(params, Stream::new(stream_id, dirty_state));
		storage.sources().sources.insert(params.clone(), source_entity);
		true
	}
	fn push(&mut self, storage: &mut S, source_entity: Entity, params: &DT::Params, stream_id: StreamId, data: &DT::Data) -> bool {
		if !Self::is_live(storage, source_entity, params, stream_id) { return false; } // stale stream, ignore
		storage.stream_mut(params).expect("The stream is live").data.push(data.clone());
		true
	}
	fn finish(&mut self, storage: &mut S, source_entity: Entity, params: &DT::Params, stream_id: StreamId) -> bool {
		if !Self::is_live(storage, source_entity, params, stream_id) { return false; } // stale stream, ignore
		storage.stream_mut(params).expect("The stream is live").finished = true;
		true
	}
	fn mark_dirty(&mut self, storage: &mut S, params: &DT::Params, last_clean_tick: u64) {
		if let Some(stream) = storage.stream_mut(params) {
			stream.dirty_state = DirtyState::Dirty { tick: last_clean_tick };
		}
	}
	fn drop_data(&mut self, storage: &mut S, nodes: &mut DataProducingNodes<DT>) {
		if storage.sources().data_to_drop.is_empty() { return; }
		for (params, sources) in std::mem::take(&mut storage.sources().data_to_drop) {
			for source_entity in sources {
				if storage.wanted(&params) || storage.sources().is_source(&params, source_entity) { continue; }
				let Ok((mut node, _)) = nodes.get_mut(source_entity) else { continue; };
				node.drop(&params);
			}
		}
	}
}

// ============================ Stream Storage ============================

/// Keeps the accepted stream of every `params`.
pub struct StreamStorage<DT: DataType> {
	streams: HashMap<DT::Params, Stream<DT>>,
	requests: StreamRequests<DT>,
	sources: StreamSources<DT>,
}

impl<DT: DataType> Default for StreamStorage<DT> {
	fn default() -> Self { Self { streams: HashMap::default(), requests: StreamRequests::default(), sources: StreamSources::default() } }
}

impl<DT: DataType> StreamRequesterStorage<DT> for StreamStorage<DT> {
	fn stream(&self, params: &DT::Params) -> Option<&Stream<DT>> { self.streams.get(params) }
	fn requests(&mut self) -> &mut StreamRequests<DT> { &mut self.requests }
	fn release(&mut self, params: &DT::Params) {
		if self.streams.remove(params).is_some() {
			self.sources.release(params);
		}
	}
}

impl<DT: DataType> StreamFulfillerStorage<DT> for StreamStorage<DT> {
	fn stream(&self, params: &DT::Params) -> Option<&Stream<DT>> { self.streams.get(params) }
	fn stream_mut(&mut self, params: &DT::Params) -> Option<&mut Stream<DT>> { self.streams.get_mut(params) }
	fn start(&mut self, params: &DT::Params, stream: Stream<DT>) { self.streams.insert(params.clone(), stream); }
	fn sources(&mut self) -> &mut StreamSources<DT> { &mut self.sources }
	fn wanted(&self, params: &DT::Params) -> bool { self.requests.is_waiting(params) }
	fn is_requested(&self, params: &DT::Params, allowed_tick: &DirtyState) -> bool { self.requests.is_waiting_for(params, allowed_tick) }
}

// ============================ Registering ============================

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
		self.register_component_as::<dyn DataProducingNode<DT>, C>().init_data_parts::<DT, StreamRequester, StreamStorage<DT>, StreamFulfiller>()
	}

	fn register_consumer_streaming<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + DataConsumingNode<DT>,
	{
		self.register_component_as::<dyn DataConsumingNode<DT>, C>().init_data_parts::<DT, StreamRequester, StreamStorage<DT>, StreamFulfiller>()
	}
}
