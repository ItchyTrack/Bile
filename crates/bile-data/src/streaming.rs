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
use bevy_trait_query::{All, One, RegisterExt};

use crate::{
	dyn_system_params::{DynParam, DynSystemParams, RegisterDynParam},
	setup::{BileDataSchedule, BileDataSystemSet, DataType, DirtyState, Request, StateTick},
};

// TODO: redo everything here. The current code is very bad.

// ============================ Nodes ============================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StreamId(u64);

impl StreamId {
	// Every request gets a new id, so that a stale stream can be told apart from the newest one and the fulfiller
	// that asked for it.
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
	requester: Query<'w, 's, (One<&'static mut dyn NodeRequester<DT>>, One<&'static mut dyn DataStorage<DT>>)>,
	fulfillers: Fulfillers<'w, 's, DT>,
}

impl<DT: DataType> DataRequester<'_, '_, DT> {
	pub fn request(&mut self, request: Request<DT>, requester_node: Entity) {
		let Ok((mut requester, mut storage)) = self.requester.single_mut() else {
			panic!("{} has no requester for nodes, a consuming node of it has to be registered.", type_name::<DT>())
		};
		requester.request(&mut *storage, &mut self.fulfillers, request, requester_node);
	}
	pub fn cancel(&mut self, params: &DT::Params, requester_node: Entity) {
		if let Ok((mut requester, mut storage)) = self.requester.single_mut() && requester.cancel(&mut *storage, params, requester_node) {
			self.fulfillers.release(&mut *storage, params);
		}
	}
}

#[derive(SystemParam)]
pub struct DataPasser<'w, 's, DT: DataType> {
	parts: Query<'w, 's, (One<&'static mut dyn DataStorage<DT>>, All<&'static mut dyn ErasedFulfiller<DT>>)>,
}

impl<DT: DataType> DataPasser<'_, '_, DT> {
	/// Producer calls this once, before any `push`.
	pub fn start(&mut self, source_entity: Entity, params: &DT::Params, stream_id: StreamId, dirty_state: DirtyState) {
		let Ok((mut storage, fulfillers)) = self.parts.single_mut() else { return; };
		let mut fulfillers: Vec<_> = fulfillers.into_iter().collect();
		// Only the fulfiller that asked for the stream starts it. Then the data no longer comes from the others.
		let Some(started) = fulfillers.iter_mut().position(|fulfiller| fulfiller.start(&mut *storage, source_entity, params, stream_id, dirty_state)) else { return; };
		for (index, fulfiller) in fulfillers.iter_mut().enumerate() {
			if index != started {
				fulfiller.release(&mut *storage, params);
			}
		}
	}
	/// Producer calls this any number of times between `start` and `finish`.
	pub fn push(&mut self, source_entity: Entity, params: &DT::Params, stream_id: StreamId, data: DT::Data) {
		let Ok((mut storage, fulfillers)) = self.parts.single_mut() else { return; };
		for mut fulfiller in fulfillers {
			if fulfiller.push(&mut *storage, source_entity, params, stream_id, &data) { return; } // only the live one can
		}
	}
	/// Producer calls this once to close out the stream.
	pub fn finish(&mut self, source_entity: Entity, params: &DT::Params, stream_id: StreamId) {
		let Ok((mut storage, fulfillers)) = self.parts.single_mut() else { return; };
		for mut fulfiller in fulfillers {
			if fulfiller.finish(&mut *storage, source_entity, params, stream_id) { return; } // the stream is complete
		}
	}
}

#[derive(SystemParam)]
pub struct DataDirtier<'w, 's, DT: DataType> {
	bile_state_tick: Res<'w, StateTick>,
	parts: Query<
		'w,
		's,
		(One<&'static mut dyn DataStorage<DT>>, Option<All<&'static mut dyn ErasedFulfiller<DT>>>, Option<All<&'static mut dyn ErasedRequester<DT>>>),
	>,
	system_params: DynSystemParams<'w, 's, Dirtying<DT>>,
}

impl<DT: DataType> DataDirtier<'_, '_, DT> {
	pub fn mark_dirty(&mut self, params: &DT::Params, diff: Option<&DT::Diff>) {
		let last_clean_tick = self.bile_state_tick.update();
		let Ok((mut storage, fulfillers, requesters)) = self.parts.single_mut() else { return; };
		for mut fulfiller in fulfillers.into_iter().flatten() {
			fulfiller.mark_dirty(&mut *storage, params, last_clean_tick);
		}
		for mut requester in requesters.into_iter().flatten() {
			requester.mark_dirty(&mut *storage, &mut self.system_params, params, last_clean_tick, diff);
		}
	}
}

/// The fulfillers of a data type. A request is fulfilled by the cheapest of them.
#[derive(SystemParam)]
pub struct Fulfillers<'w, 's, DT: DataType> {
	fulfillers: Query<'w, 's, All<&'static mut dyn ErasedFulfiller<DT>>>,
	system_params: DynSystemParams<'w, 's, Fulfilling<DT>>,
}

impl<DT: DataType> Fulfillers<'_, '_, DT> {
	/// Has the data streamed, unless it already is.
	pub fn fulfill(&mut self, storage: &mut dyn DataStorage<DT>, request: Request<DT>) {
		let Ok(fulfillers) = self.fulfillers.single_mut() else { panic!("{} has no fulfillers, a producing node of it has to be registered.", type_name::<DT>()) };
		let mut fulfillers: Vec<_> = fulfillers.into_iter().collect();
		let mut cheapest: Option<(f32, usize)> = None;
		for (index, fulfiller) in fulfillers.iter().enumerate() {
			if let Some(cost) = fulfiller.cost(storage, &mut self.system_params, &request) && cheapest.is_none_or(|(cheapest, _)| cost < cheapest) {
				cheapest = Some((cost, index));
			}
		}
		let (_, index) = cheapest.expect("Data request failed to find node to fulfill the request. This should never happen!");
		fulfillers[index].fulfill(storage, &mut self.system_params, request);
	}
	/// The storage dropped the stream, so the nodes it came from are dropped.
	pub fn release(&mut self, storage: &mut dyn DataStorage<DT>, params: &DT::Params) {
		for mut fulfiller in self.fulfillers.iter_mut().flatten() {
			fulfiller.release(storage, params);
		}
	}
}

/// What the entry points need the params of the parts for.
pub struct Fulfilling<DT>(PhantomData<fn() -> DT>);
pub struct Dirtying<DT>(PhantomData<fn() -> DT>);

// ============================ Parts ============================

/// Streams the data from the storage to whoever asks for it.
pub trait Requester<DT: DataType>: DataPart<DT> {
	type Param: SystemParam + 'static; // to reach whoever has the data

	fn mark_dirty(&mut self, storage: &mut Self::Storage, param: &mut SystemParamItem<Self::Param>, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>);
}

/// Gets the streams into the storage from its kind of producing nodes.
pub trait Fulfiller<DT: DataType>: DataPart<DT> {
	type Param: SystemParam + 'static; // to reach the producing nodes

	/// None if it can not fulfill the request.
	fn cost(&self, storage: &Self::Storage, param: &mut SystemParamItem<Self::Param>, request: &Request<DT>) -> Option<f32>;
	/// Has the data streamed, unless it already is.
	fn fulfill(&mut self, storage: &mut Self::Storage, param: &mut SystemParamItem<Self::Param>, request: Request<DT>);
	/// Returns true if it started a new stream. Only the fulfiller that asked for the stream can.
	fn start(&mut self, storage: &mut Self::Storage, source_entity: Entity, params: &DT::Params, stream_id: StreamId, dirty_state: DirtyState) -> bool;
	/// Returns true if the data was pushed to the stream.
	fn push(&mut self, storage: &mut Self::Storage, source_entity: Entity, params: &DT::Params, stream_id: StreamId, data: &DT::Data) -> bool;
	/// Returns true if the stream is complete.
	fn finish(&mut self, storage: &mut Self::Storage, source_entity: Entity, params: &DT::Params, stream_id: StreamId) -> bool;
	fn mark_dirty(&mut self, storage: &mut Self::Storage, params: &DT::Params, last_clean_tick: u64);
	/// The stream no longer comes from the nodes of this fulfiller, it was dropped or another fulfiller started one.
	fn release(&mut self, storage: &mut Self::Storage, params: &DT::Params);
	/// Drops the nodes the stored streams no longer come from.
	fn drop_data(&mut self, storage: &Self::Storage, param: &mut SystemParamItem<Self::Param>);
}

/// Adds what every requester needs, for its `DataPart::setup`.
pub fn setup_requester<DT: DataType, R: Requester<DT>>(app: &mut App) {
	app.register_component_as::<dyn ErasedRequester<DT>, R>().register_dyn_param::<Dirtying<DT>, R>();
}

/// Adds what every fulfiller needs, for its `DataPart::setup`.
pub fn setup_fulfiller<DT: DataType, F: Fulfiller<DT>>(app: &mut App) {
	app.register_component_as::<dyn ErasedFulfiller<DT>, F>()
		.register_dyn_param::<Fulfilling<DT>, F>()
		.add_systems(BileDataSchedule, drop_data::<DT, F>.in_set(BileDataSystemSet::DropData));
}

fn drop_data<DT: DataType, F: Fulfiller<DT>>(mut parts: Query<(&mut F, &F::Storage)>, mut param: StaticSystemParam<F::Param>) {
	for (mut fulfiller, storage) in &mut parts {
		fulfiller.drop_data(storage, &mut param);
	}
}

// The entry points reach the parts through these, they do not know their types.

/// The requester `DataRequester` reaches, the one of the consuming nodes.
#[bevy_trait_query::queryable]
pub trait NodeRequester<DT: DataType> {
	fn request(&mut self, storage: &mut dyn DataStorage<DT>, fulfillers: &mut Fulfillers<DT>, request: Request<DT>, requester_node: Entity);
	/// Returns true if the storage dropped the stream.
	fn cancel(&mut self, storage: &mut dyn DataStorage<DT>, params: &DT::Params, requester_node: Entity) -> bool;
}

#[bevy_trait_query::queryable]
pub trait ErasedRequester<DT: DataType> {
	fn mark_dirty(&mut self, storage: &mut dyn DataStorage<DT>, system_params: &mut DynSystemParams<Dirtying<DT>>, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>);
}

impl<DT: DataType, R: Requester<DT>> ErasedRequester<DT> for R {
	fn mark_dirty(&mut self, storage: &mut dyn DataStorage<DT>, system_params: &mut DynSystemParams<Dirtying<DT>>, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>) {
		<R as Requester<DT>>::mark_dirty(self, storage.downcast_mut(), &mut system_params.get::<R>(), params, last_clean_tick, diff);
	}
}

impl<DT: DataType, R: Requester<DT>> DynParam<Dirtying<DT>> for R {
	type Param = R::Param;
}

#[bevy_trait_query::queryable]
pub trait ErasedFulfiller<DT: DataType> {
	fn cost(&self, storage: &dyn DataStorage<DT>, system_params: &mut DynSystemParams<Fulfilling<DT>>, request: &Request<DT>) -> Option<f32>;
	fn fulfill(&mut self, storage: &mut dyn DataStorage<DT>, system_params: &mut DynSystemParams<Fulfilling<DT>>, request: Request<DT>);
	fn start(&mut self, storage: &mut dyn DataStorage<DT>, source_entity: Entity, params: &DT::Params, stream_id: StreamId, dirty_state: DirtyState) -> bool;
	fn push(&mut self, storage: &mut dyn DataStorage<DT>, source_entity: Entity, params: &DT::Params, stream_id: StreamId, data: &DT::Data) -> bool;
	fn finish(&mut self, storage: &mut dyn DataStorage<DT>, source_entity: Entity, params: &DT::Params, stream_id: StreamId) -> bool;
	fn mark_dirty(&mut self, storage: &mut dyn DataStorage<DT>, params: &DT::Params, last_clean_tick: u64);
	fn release(&mut self, storage: &mut dyn DataStorage<DT>, params: &DT::Params);
}

impl<DT: DataType, F: Fulfiller<DT>> ErasedFulfiller<DT> for F {
	fn cost(&self, storage: &dyn DataStorage<DT>, system_params: &mut DynSystemParams<Fulfilling<DT>>, request: &Request<DT>) -> Option<f32> {
		<F as Fulfiller<DT>>::cost(self, storage.downcast_ref(), &mut system_params.get::<F>(), request)
	}
	fn fulfill(&mut self, storage: &mut dyn DataStorage<DT>, system_params: &mut DynSystemParams<Fulfilling<DT>>, request: Request<DT>) {
		<F as Fulfiller<DT>>::fulfill(self, storage.downcast_mut(), &mut system_params.get::<F>(), request);
	}
	fn start(&mut self, storage: &mut dyn DataStorage<DT>, source_entity: Entity, params: &DT::Params, stream_id: StreamId, dirty_state: DirtyState) -> bool {
		<F as Fulfiller<DT>>::start(self, storage.downcast_mut(), source_entity, params, stream_id, dirty_state)
	}
	fn push(&mut self, storage: &mut dyn DataStorage<DT>, source_entity: Entity, params: &DT::Params, stream_id: StreamId, data: &DT::Data) -> bool {
		<F as Fulfiller<DT>>::push(self, storage.downcast_mut(), source_entity, params, stream_id, data)
	}
	fn finish(&mut self, storage: &mut dyn DataStorage<DT>, source_entity: Entity, params: &DT::Params, stream_id: StreamId) -> bool {
		<F as Fulfiller<DT>>::finish(self, storage.downcast_mut(), source_entity, params, stream_id)
	}
	fn mark_dirty(&mut self, storage: &mut dyn DataStorage<DT>, params: &DT::Params, last_clean_tick: u64) {
		<F as Fulfiller<DT>>::mark_dirty(self, storage.downcast_mut(), params, last_clean_tick);
	}
	fn release(&mut self, storage: &mut dyn DataStorage<DT>, params: &DT::Params) { <F as Fulfiller<DT>>::release(self, storage.downcast_mut(), params); }
}

impl<DT: DataType, F: Fulfiller<DT>> DynParam<Fulfilling<DT>> for F {
	type Param = F::Param;
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
pub trait StreamRequesterStorage<DT: DataType>: Storage<DT> {
	fn stream(&self, params: &DT::Params) -> Option<&Stream<DT>>;
	fn requests(&mut self) -> &mut StreamRequests<DT>;
	/// No node uses or waits for the stream anymore. Returns true if the stream was dropped.
	fn release(&mut self, params: &DT::Params) -> bool;
}

/// The requests of the consuming nodes, kept in the storage.
pub struct StreamRequests<DT: DataType> {
	waiting: HashMap<DT::Params, HashMap<Entity, DirtyState>>, // and how fresh the stream they wait for has to be
	usages: HashMap<DT::Params, HashSet<Entity>>,               // they get the stream that is accepted
	// Ordered (unlike the simple requests' `finished`) because start/push.../finish must be delivered in that order.
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
	/// The storage accepted a new stream, the nodes it satisfies get it instead of the one they had.
	pub fn started(&mut self, params: &DT::Params, dirty_state: DirtyState) {
		let waiting = self.waiting.remove(params).unwrap_or_default();
		let (usages, still_waiting): (HashMap<_, _>, HashMap<_, _>) = waiting.into_iter().partition(|(_, required)| dirty_state.satisfies(required));
		// the users of a replaced stream do not get the new one
		self.usages.remove(params);
		if !still_waiting.is_empty() {
			self.waiting.insert(params.clone(), still_waiting);
		}
		if !usages.is_empty() {
			self.usages.insert(params.clone(), usages.into_keys().collect());
			self.send(params, |entities| StreamEvent::Start { entities });
		}
	}
	/// The storage pushed data to the stream.
	pub fn pushed(&mut self, params: &DT::Params, data: &DT::Data) { self.send(params, |entities| StreamEvent::Push { entities, data: data.clone() }); }
	/// The stream is complete.
	pub fn finished(&mut self, params: &DT::Params) { self.send(params, |entities| StreamEvent::Finish { entities }); }
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

/// Streams the data to the consuming nodes that request it. A node that requests a stream that is already accepted
/// gets what was already streamed and then the rest.
#[derive(Component)]
pub struct StreamRequester<DT, S>(PhantomData<fn() -> (DT, S)>);

impl<DT, S> Default for StreamRequester<DT, S> {
	fn default() -> Self { Self(PhantomData) }
}

impl<DT: DataType, S: StreamRequesterStorage<DT>> StreamRequester<DT, S> {
	// Passes what was streamed to the nodes, in order.
	fn pass_data(storage: &mut S, nodes: &mut DataConsumingNodes<DT>, current_tick: u64) {
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

impl<DT: DataType, S: StreamRequesterStorage<DT>> DataPart<DT> for StreamRequester<DT, S> {
	type Storage = S;

	fn setup(app: &mut App) {
		setup_requester::<DT, Self>(app);
		app.register_component_as::<dyn NodeRequester<DT>, Self>()
			.add_systems(BileDataSchedule, pass_data::<DT, S>.in_set(BileDataSystemSet::PassData));
	}
}

impl<DT: DataType, S: StreamRequesterStorage<DT>> Requester<DT> for StreamRequester<DT, S> {
	type Param = DataConsumingNodes<'static, 'static, DT>;

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
}

impl<DT: DataType, S: StreamRequesterStorage<DT>> NodeRequester<DT> for StreamRequester<DT, S> {
	fn request(&mut self, storage: &mut dyn DataStorage<DT>, fulfillers: &mut Fulfillers<DT>, request: Request<DT>, requester_node: Entity) {
		let storage = storage.downcast_mut::<S>();
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
		fulfillers.fulfill(storage, request);
		storage.requests().waiting.entry(params).or_default().insert(requester_node, allowed_tick);
	}
	fn cancel(&mut self, storage: &mut dyn DataStorage<DT>, params: &DT::Params, requester_node: Entity) -> bool {
		let storage = storage.downcast_mut::<S>();
		let requests = storage.requests();
		requests.forget(params, requester_node);
		!requests.is_requested(params) && storage.release(params)
	}
}

fn pass_data<DT: DataType, S: StreamRequesterStorage<DT>>(
	bile_state_tick: Res<StateTick>,
	mut storages: Query<&mut S, With<StreamRequester<DT, S>>>,
	mut nodes: StaticSystemParam<DataConsumingNodes<DT>>,
) {
	for mut storage in &mut storages {
		StreamRequester::<DT, S>::pass_data(&mut storage, &mut nodes, bile_state_tick.current_tick());
	}
}

// ============================ Stream Fulfiller ============================

/// What the stream fulfillers need from a storage.
pub trait StreamFulfillerStorage<DT: DataType>: Storage<DT> {
	fn stream(&self, params: &DT::Params) -> Option<&Stream<DT>>;
	/// To change how fresh the stream is. Its data is changed with `push` and `finish`.
	fn stream_mut(&mut self, params: &DT::Params) -> Option<&mut Stream<DT>>;
	/// Replaces the stream that was accepted, live or not.
	fn start(&mut self, params: &DT::Params, stream: Stream<DT>);
	fn push(&mut self, params: &DT::Params, data: &DT::Data);
	fn finish(&mut self, params: &DT::Params);
	/// Something waits for the stream, so it is accepted when it starts.
	fn wanted(&self, params: &DT::Params) -> bool;
	/// A stream that satisfies `allowed_tick` is already on its way.
	fn is_requested(&self, params: &DT::Params, allowed_tick: &DirtyState) -> bool;
}

/// Has the cheapest node stream the data.
#[derive(Component)]
pub struct StreamFulfiller<DT: DataType, S> {
	requested: HashMap<DT::Params, StreamId>, // the newest stream a node was asked for, until it starts
	sources: HashMap<DT::Params, Entity>,     // the node of the accepted stream
	data_to_drop: HashMap<DT::Params, HashSet<Entity>>,
	_marker: PhantomData<fn() -> S>,
}

impl<DT: DataType, S> Default for StreamFulfiller<DT, S> {
	fn default() -> Self { Self { requested: HashMap::default(), sources: HashMap::default(), data_to_drop: HashMap::default(), _marker: PhantomData } }
}

impl<DT: DataType, S: StreamFulfillerStorage<DT>> StreamFulfiller<DT, S> {
	fn drop_source(&mut self, params: &DT::Params, source_entity: Entity) { self.data_to_drop.entry(params.clone()).or_default().insert(source_entity); }
	fn is_source(&self, params: &DT::Params, source_entity: Entity) -> bool { self.sources.get(params) == Some(&source_entity) }
	fn is_live(&self, storage: &S, source_entity: Entity, params: &DT::Params, stream_id: StreamId) -> bool {
		self.is_source(params, source_entity) && storage.stream(params).is_some_and(|stream| stream.stream_id == stream_id && !stream.finished)
	}
}

impl<DT: DataType, S: StreamFulfillerStorage<DT>> DataPart<DT> for StreamFulfiller<DT, S> {
	type Storage = S;

	fn setup(app: &mut App) { setup_fulfiller::<DT, Self>(app); }
}

impl<DT: DataType, S: StreamFulfillerStorage<DT>> Fulfiller<DT> for StreamFulfiller<DT, S> {
	type Param = DataProducingNodes<'static, 'static, DT>;

	fn cost(&self, _storage: &S, nodes: &mut DataProducingNodes<DT>, request: &Request<DT>) -> Option<f32> { best_node(nodes, request).map(|(cost, _)| cost) }
	fn fulfill(&mut self, storage: &mut S, nodes: &mut DataProducingNodes<DT>, request: Request<DT>) {
		if storage.is_requested(&request.params, &request.allowed_tick) { return; }
		let node_entity = best_node(nodes, &request).expect("Data request failed to find node to fulfill the request. This should never happen!").1;
		if let Some(dropped) = self.data_to_drop.get_mut(&request.params) {
			dropped.remove(&node_entity);
		}
		let stream_id = StreamId::new();
		self.requested.insert(request.params.clone(), stream_id);
		nodes.get_mut(node_entity).expect("This must exist as it was found in best_node").0.request(request, stream_id);
	}
	fn start(&mut self, storage: &mut S, source_entity: Entity, params: &DT::Params, stream_id: StreamId, dirty_state: DirtyState) -> bool {
		if self.requested.get(params) != Some(&stream_id) { return false; } // not the newest stream of this fulfiller
		self.requested.remove(params);
		let existing = storage.stream(params).map(|stream| (stream.dirty_state, stream.finished));
		// something fresher is already active or already finished here, or no one wants this anymore
		if existing.is_some_and(|(existing, _)| existing.is_fresher_than(&dirty_state)) || !storage.wanted(params) {
			self.drop_source(params, source_entity);
			return false;
		}
		// supersede whatever was there, live or not; if it was still live, tell it to stop too
		if let Some((_, finished)) = existing && let Some(old_source) = self.sources.get(params).copied() && (!finished || old_source != source_entity) {
			self.drop_source(params, old_source);
		}
		storage.start(params, Stream::new(stream_id, dirty_state));
		self.sources.insert(params.clone(), source_entity);
		true
	}
	fn push(&mut self, storage: &mut S, source_entity: Entity, params: &DT::Params, stream_id: StreamId, data: &DT::Data) -> bool {
		if !self.is_live(storage, source_entity, params, stream_id) { return false; } // stale stream, ignore
		storage.push(params, data);
		true
	}
	fn finish(&mut self, storage: &mut S, source_entity: Entity, params: &DT::Params, stream_id: StreamId) -> bool {
		if !self.is_live(storage, source_entity, params, stream_id) { return false; } // stale stream, ignore
		storage.finish(params);
		true
	}
	fn mark_dirty(&mut self, storage: &mut S, params: &DT::Params, last_clean_tick: u64) {
		if let Some(stream) = storage.stream_mut(params) {
			stream.dirty_state = DirtyState::Dirty { tick: last_clean_tick };
		}
	}
	fn release(&mut self, _storage: &mut S, params: &DT::Params) {
		if let Some(source_entity) = self.sources.remove(params) {
			self.drop_source(params, source_entity);
		}
	}
	fn drop_data(&mut self, storage: &S, nodes: &mut DataProducingNodes<DT>) {
		if self.data_to_drop.is_empty() { return; }
		for (params, sources) in std::mem::take(&mut self.data_to_drop) {
			for source_entity in sources {
				if storage.wanted(&params) || self.is_source(&params, source_entity) { continue; }
				let Ok((mut node, _)) = nodes.get_mut(source_entity) else { continue; };
				node.drop(&params);
			}
		}
	}
}

// ============================ Stream Storage ============================

/// Keeps the accepted stream of every `params`.
#[derive(Component)]
pub struct StreamStorage<DT: DataType> {
	streams: HashMap<DT::Params, Stream<DT>>,
	requests: StreamRequests<DT>,
}

impl<DT: DataType> Default for StreamStorage<DT> {
	fn default() -> Self { Self { streams: HashMap::default(), requests: StreamRequests::default() } }
}

impl<DT: DataType> Storage<DT> for StreamStorage<DT> {}

impl<DT: DataType> StreamRequesterStorage<DT> for StreamStorage<DT> {
	fn stream(&self, params: &DT::Params) -> Option<&Stream<DT>> { self.streams.get(params) }
	fn requests(&mut self) -> &mut StreamRequests<DT> { &mut self.requests }
	fn release(&mut self, params: &DT::Params) -> bool { self.streams.remove(params).is_some() }
}

impl<DT: DataType> StreamFulfillerStorage<DT> for StreamStorage<DT> {
	fn stream(&self, params: &DT::Params) -> Option<&Stream<DT>> { self.streams.get(params) }
	fn stream_mut(&mut self, params: &DT::Params) -> Option<&mut Stream<DT>> { self.streams.get_mut(params) }
	fn start(&mut self, params: &DT::Params, stream: Stream<DT>) {
		self.requests.started(params, stream.dirty_state);
		self.streams.insert(params.clone(), stream);
	}
	fn push(&mut self, params: &DT::Params, data: &DT::Data) {
		let Some(stream) = self.streams.get_mut(params) else { return; };
		stream.data.push(data.clone());
		self.requests.pushed(params, data);
	}
	fn finish(&mut self, params: &DT::Params) {
		let Some(stream) = self.streams.get_mut(params) else { return; };
		stream.finished = true;
		self.requests.finished(params);
	}
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
		self.register_component_as::<dyn DataProducingNode<DT>, C>().init_data_part::<DT, StreamFulfiller<DT, StreamStorage<DT>>>()
	}

	fn register_consumer_streaming<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + DataConsumingNode<DT>,
	{
		self.register_component_as::<dyn DataConsumingNode<DT>, C>().init_data_part::<DT, StreamRequester<DT, StreamStorage<DT>>>()
	}
}
