use std::{
	any::type_name,
	collections::{HashMap, HashSet},
	marker::PhantomData,
};

use bevy::{
	ecs::system::{StaticSystemParam, SystemParam, SystemParamItem},
	prelude::*,
};
use bevy_trait_query::{All, One, RegisterExt};

use crate::{
	diff::{self, DiffApplyingNodes},
	dyn_system_params::{DynParam, DynSystemParams, RegisterDynParam},
	setup::{BileDataSchedule, BileDataSystemSet, DataType, DirtyState, Request, StateTick},
};

// ============================ Nodes ============================



// None if no node can fulfill the request
pub fn best_node<DT: DataType>(data_producing_nodes: &DataProducingNodes<DT>, request: &Request<DT>) -> Option<(f32, Entity)> {
	data_producing_nodes.iter().filter_map(|(node, entity)| Some((node.cost(request)?, entity))).min_by(|a, b| a.0.total_cmp(&b.0))
}

// ============================ Entry points ============================





/// The fulfillers of a data type. A request is fulfilled by the cheapest of them.
#[derive(SystemParam)]
pub struct Fulfillers<'w, 's, DT: DataType> {
	fulfillers: Query<'w, 's, All<&'static mut dyn ErasedFulfiller<DT>>>,
	system_params: DynSystemParams<'w, 's, Fulfilling<DT>>,
}

impl<DT: DataType> Fulfillers<'_, '_, DT> {
	/// Returns true if the data is in the storage and satisfies the request right away. Otherwise it is passed later.
	pub fn fulfill(&mut self, storage: &mut dyn DataStorage<DT>, request: Request<DT>) -> bool {
		let Ok(fulfillers) = self.fulfillers.single_mut() else { panic!("{} has no fulfillers, a producing node of it has to be registered.", type_name::<DT>()) };
		let mut fulfillers: Vec<_> = fulfillers.into_iter().collect();
		let mut cheapest: Option<(f32, usize)> = None;
		for (index, fulfiller) in fulfillers.iter().enumerate() {
			if let Some(cost) = fulfiller.cost(storage, &mut self.system_params, &request) && cheapest.is_none_or(|(cheapest, _)| cost < cheapest) {
				cheapest = Some((cost, index));
			}
		}
		let (_, index) = cheapest.expect("Data request failed to find node to fulfill the request. This should never happen!");
		fulfillers[index].fulfill(storage, &mut self.system_params, request)
	}
	/// The storage dropped the data, so the nodes it came from are dropped.
	pub fn release(&mut self, storage: &mut dyn DataStorage<DT>, params: &DT::Params) {
		for mut fulfiller in self.fulfillers.iter_mut().flatten() {
			fulfiller.release(storage, params);
		}
	}
}

/// What the entry points need the params of the parts for.
pub struct Requesting<DT>(PhantomData<fn() -> DT>);
pub struct Fulfilling<DT>(PhantomData<fn() -> DT>);
pub struct Dirtying<DT>(PhantomData<fn() -> DT>);

// ============================ Parts ============================

/// Brings the data from the storage to whoever asks for it.
pub trait Requester<DT: DataType>: DataPart<DT> {
	type Param: SystemParam + 'static; // to reach whoever has the data

	fn mark_dirty(&mut self, storage: &mut Self::Storage, param: &mut SystemParamItem<Self::Param>, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>);
}

/// Gets the data into the storage from its kind of producing nodes.
pub trait Fulfiller<DT: DataType>: DataPart<DT> {
	type Param: SystemParam + 'static; // to reach the producing nodes

	/// None if it can not fulfill the request.
	fn cost(&self, storage: &Self::Storage, param: &mut SystemParamItem<Self::Param>, request: &Request<DT>) -> Option<f32>;
	/// Brings the data up to date right away if it can and returns true, or else has a node produce it.
	fn fulfill(&mut self, storage: &mut Self::Storage, param: &mut SystemParamItem<Self::Param>, request: Request<DT>) -> bool;
	/// If the node passes data this fulfiller asked it for.
	fn is_source(&self, params: &DT::Params, source_entity: Entity) -> bool;
	/// A node passed data. Returns true if it was stored.
	fn pass_data(&mut self, storage: &mut Self::Storage, source_entity: Entity, params: &DT::Params, dirty_state: DirtyState, data: DT::Data) -> bool;
	fn mark_dirty(&mut self, storage: &mut Self::Storage, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>);
	/// The data no longer comes from the nodes of this fulfiller, it was dropped or another fulfiller stored it.
	fn release(&mut self, storage: &mut Self::Storage, params: &DT::Params);
	/// Drops the nodes the stored data no longer comes from.
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
	fn fulfill(&mut self, storage: &mut dyn DataStorage<DT>, system_params: &mut DynSystemParams<Fulfilling<DT>>, request: Request<DT>) -> bool;
	fn is_source(&self, params: &DT::Params, source_entity: Entity) -> bool;
	fn pass_data(&mut self, storage: &mut dyn DataStorage<DT>, source_entity: Entity, params: &DT::Params, dirty_state: DirtyState, data: DT::Data) -> bool;
	fn mark_dirty(&mut self, storage: &mut dyn DataStorage<DT>, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>);
	fn release(&mut self, storage: &mut dyn DataStorage<DT>, params: &DT::Params);
}

impl<DT: DataType, F: Fulfiller<DT>> ErasedFulfiller<DT> for F {
	fn cost(&self, storage: &dyn DataStorage<DT>, system_params: &mut DynSystemParams<Fulfilling<DT>>, request: &Request<DT>) -> Option<f32> {
		<F as Fulfiller<DT>>::cost(self, storage.downcast_ref(), &mut system_params.get::<F>(), request)
	}
	fn fulfill(&mut self, storage: &mut dyn DataStorage<DT>, system_params: &mut DynSystemParams<Fulfilling<DT>>, request: Request<DT>) -> bool {
		<F as Fulfiller<DT>>::fulfill(self, storage.downcast_mut(), &mut system_params.get::<F>(), request)
	}
	fn is_source(&self, params: &DT::Params, source_entity: Entity) -> bool { <F as Fulfiller<DT>>::is_source(self, params, source_entity) }
	fn pass_data(&mut self, storage: &mut dyn DataStorage<DT>, source_entity: Entity, params: &DT::Params, dirty_state: DirtyState, data: DT::Data) -> bool {
		<F as Fulfiller<DT>>::pass_data(self, storage.downcast_mut(), source_entity, params, dirty_state, data)
	}
	fn mark_dirty(&mut self, storage: &mut dyn DataStorage<DT>, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>) {
		<F as Fulfiller<DT>>::mark_dirty(self, storage.downcast_mut(), params, last_clean_tick, diff);
	}
	fn release(&mut self, storage: &mut dyn DataStorage<DT>, params: &DT::Params) { <F as Fulfiller<DT>>::release(self, storage.downcast_mut(), params); }
}

impl<DT: DataType, F: Fulfiller<DT>> DynParam<Fulfilling<DT>> for F {
	type Param = F::Param;
}

// ============================ Simple Requester ============================

/// What the simple requester needs from a storage.
pub trait SimpleRequesterStorage<DT: DataType>: Storage<DT> {
	type Param: SystemParam + 'static; // to read the data

	fn data(&self, param: &SystemParamItem<Self::Param>, params: &DT::Params) -> Option<(DirtyState, DT::Data)>;
	fn requests(&mut self) -> &mut Requests<DT>;
	/// No node uses or waits for the data anymore. Returns true if the data was dropped.
	fn release(&mut self, params: &DT::Params) -> bool;
}

/// The requests of the consuming nodes, kept in the storage.
pub struct Requests<DT: DataType> {
	waiting: HashMap<DT::Params, HashMap<Entity, DirtyState>>, // and how fresh the data they wait for has to be
	finished: HashMap<DT::Params, HashSet<Entity>>,             // the data was stored for them and is passed to them next
	usages: HashMap<DT::Params, HashSet<Entity>>,               // they have the data
}

impl<DT: DataType> Default for Requests<DT> {
	fn default() -> Self { Self { waiting: HashMap::default(), finished: HashMap::default(), usages: HashMap::default() } }
}

impl<DT: DataType> Requests<DT> {
	pub fn is_waiting(&self, params: &DT::Params) -> bool { self.waiting.contains_key(params) }
	/// If data that satisfies `allowed_tick` is already on its way.
	pub fn is_waiting_for(&self, params: &DT::Params, allowed_tick: &DirtyState) -> bool {
		self.waiting.get(params).is_some_and(|waiting| waiting.values().any(|required| required.satisfies(allowed_tick)))
	}
	pub fn is_requested(&self, params: &DT::Params) -> bool {
		self.waiting.contains_key(params) || self.finished.contains_key(params) || self.usages.contains_key(params)
	}
	/// The storage stored the data, the nodes it satisfies get it next.
	pub fn stored(&mut self, params: &DT::Params, dirty_state: DirtyState) {
		let Some(waiting) = self.waiting.remove(params) else { return; };
		let (satisfied, still_waiting): (HashMap<_, _>, HashMap<_, _>) = waiting.into_iter().partition(|(_, required)| dirty_state.satisfies(required));
		if !satisfied.is_empty() {
			self.finished.entry(params.clone()).or_default().extend(satisfied.into_keys());
		}
		if !still_waiting.is_empty() {
			self.waiting.insert(params.clone(), still_waiting);
		}
	}
	fn forget(&mut self, params: &DT::Params, requester_node: Entity) {
		if self.waiting.get_mut(params).is_some_and(|waiting| { waiting.remove(&requester_node); waiting.is_empty() }) {
			self.waiting.remove(params);
		}
		if self.finished.get_mut(params).is_some_and(|finished| { finished.remove(&requester_node); finished.is_empty() }) {
			self.finished.remove(params);
		}
		if self.usages.get_mut(params).is_some_and(|usages| { usages.remove(&requester_node); usages.is_empty() }) {
			self.usages.remove(params);
		}
	}
}

/// Passes single pieces of data to the consuming nodes that request them.
#[derive(Component)]
pub struct SimpleRequester<DT, S>(PhantomData<fn() -> (DT, S)>);

impl<DT, S> Default for SimpleRequester<DT, S> {
	fn default() -> Self { Self(PhantomData) }
}

impl<DT: DataType, S: SimpleRequesterStorage<DT>> SimpleRequester<DT, S> {
	fn stored_data(storage: &S, param: &SystemParamItem<S::Param>, request: &Request<DT>) -> Option<DT::Data> {
		storage.data(param, &request.params).filter(|(dirty_state, _)| dirty_state.satisfies(&request.allowed_tick)).map(|(_, data)| data)
	}
	// Passes the stored data to the nodes it was stored for.
	fn pass_data(storage: &mut S, param: &SystemParamItem<S::Param>, nodes: &mut DataConsumingNodes<DT>, current_tick: u64) {
		if storage.requests().finished.is_empty() { return; }
		for (params, entities) in std::mem::take(&mut storage.requests().finished) {
			let Some((dirty_state, data)) = storage.data(param, &params) else { continue; };
			let tick = match dirty_state { DirtyState::Dirty { tick } => tick, DirtyState::UpToDate => current_tick };
			for entity in entities {
				let Ok(mut node) = nodes.get_mut(entity) else { continue; };
				storage.requests().usages.entry(params.clone()).or_default().insert(entity);
				node.pass_data(&params, tick, data.clone());
			}
		}
	}
}

impl<DT: DataType, S: SimpleRequesterStorage<DT>> DataPart<DT> for SimpleRequester<DT, S> {
	type Storage = S;

	fn setup(app: &mut App) {
		setup_requester::<DT, Self>(app);
		app.register_component_as::<dyn NodeRequester<DT>, Self>()
			.register_dyn_param::<Requesting<DT>, Self>()
			.add_systems(BileDataSchedule, pass_data::<DT, S>.in_set(BileDataSystemSet::PassData));
	}
}

impl<DT: DataType, S: SimpleRequesterStorage<DT>> Requester<DT> for SimpleRequester<DT, S> {
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

impl<DT: DataType, S: SimpleRequesterStorage<DT>> NodeRequester<DT> for SimpleRequester<DT, S> {
	fn request(
		&mut self,
		storage: &mut dyn DataStorage<DT>,
		system_params: &mut DynSystemParams<Requesting<DT>>,
		fulfillers: &mut Fulfillers<DT>,
		request: Request<DT>,
		requester_node: Entity,
	) -> Option<DT::Data> {
		let (storage, param) = (storage.downcast_mut::<S>(), &mut system_params.get::<Self>());
		let mut data = Self::stored_data(storage, param, &request);
		if data.is_none() {
			let (params, allowed_tick) = (request.params.clone(), request.allowed_tick);
			if fulfillers.fulfill(storage, Request::new(params.clone(), allowed_tick)) {
				data = Self::stored_data(storage, param, &request);
			}
			if data.is_none() {
				storage.requests().waiting.entry(params).or_default().insert(requester_node, allowed_tick);
				return None;
			}
		}
		storage.requests().usages.entry(request.params).or_default().insert(requester_node);
		data
	}
	fn cancel(&mut self, storage: &mut dyn DataStorage<DT>, params: &DT::Params, requester_node: Entity) -> bool {
		let storage = storage.downcast_mut::<S>();
		let requests = storage.requests();
		requests.forget(params, requester_node);
		!requests.is_requested(params) && storage.release(params)
	}
}

impl<DT: DataType, S: SimpleRequesterStorage<DT>> DynParam<Requesting<DT>> for SimpleRequester<DT, S> {
	type Param = S::Param;
}

fn pass_data<DT: DataType, S: SimpleRequesterStorage<DT>>(
	bile_state_tick: Res<StateTick>,
	mut storages: Query<&mut S, With<SimpleRequester<DT, S>>>,
	param: StaticSystemParam<S::Param>,
	mut nodes: StaticSystemParam<DataConsumingNodes<DT>>,
) {
	for mut storage in &mut storages {
		SimpleRequester::<DT, S>::pass_data(&mut storage, &param, &mut nodes, bile_state_tick.current_tick());
	}
}

// ============================ Simple Fulfiller ============================

/// What the simple fulfiller needs from a storage.
pub trait SimpleFulfillerStorage<DT: DataType>: Storage<DT> {
	type Param: SystemParam + 'static; // to read the data

	fn data(&self, param: &SystemParamItem<Self::Param>, params: &DT::Params) -> Option<(DirtyState, DT::Data)>;
	fn dirty_state(&self, params: &DT::Params) -> Option<DirtyState>; // None if nothing is stored
	/// Something waits for the data, so it is stored when it is passed.
	fn wanted(&self, params: &DT::Params) -> bool;
	/// Data that satisfies `allowed_tick` is already on its way.
	fn is_requested(&self, params: &DT::Params, allowed_tick: &DirtyState) -> bool;
	/// None if the data is out of date and not worth keeping.
	fn store(&mut self, params: &DT::Params, dirty_state: DirtyState, data: Option<DT::Data>);
	/// Returns false if nothing is stored.
	fn mark_dirty(&mut self, params: &DT::Params, last_clean_tick: u64) -> bool;
}

/// Gets single pieces of data from the cheapest node, or applies the diffs since it was made if that is cheaper.
#[derive(Component)]
pub struct SimpleFulfiller<DT: DataType, S> {
	sources: HashMap<DT::Params, Source<DT>>,
	requested: HashMap<DT::Params, HashSet<Entity>>,    // the nodes that were asked for the data, until they pass it
	data_to_drop: HashMap<DT::Params, HashSet<Entity>>, // this data is no longer requested from these. Either no one wants it or something else provided it.
	_marker: PhantomData<fn() -> S>,
}

struct Source<DT: DataType> {
	entity: Entity,
	chain: Vec<Entity>,           // earlier sources, whose out of date data is not stored
	diffs: Option<Vec<DT::Diff>>, // every dirty since the data was made. None if one had no diff
}

impl<DT: DataType, S> Default for SimpleFulfiller<DT, S> {
	fn default() -> Self { Self { sources: HashMap::default(), requested: HashMap::default(), data_to_drop: HashMap::default(), _marker: PhantomData } }
}

impl<DT: DataType, S: SimpleFulfillerStorage<DT>> SimpleFulfiller<DT, S> {
	fn uses(&self, params: &DT::Params, entity: Entity) -> bool {
		self.sources.get(params).is_some_and(|source| source.entity == entity || source.chain.contains(&entity))
	}
	fn keep(&mut self, params: &DT::Params, entity: Entity) {
		if let Some(sources) = self.data_to_drop.get_mut(params) {
			sources.remove(&entity);
		}
	}
	// The data of a diff applier lacks what was dirtied after it got its diffs. Returns false if it does.
	fn pass(&mut self, params: &DT::Params, entity: Entity, dirty_state: DirtyState) -> bool {
		let diffs = matches!(dirty_state, DirtyState::UpToDate).then(Vec::new);
		let Some(source) = self.sources.get_mut(params) else {
			self.sources.insert(params.clone(), Source { entity, chain: Vec::new(), diffs });
			self.keep(params, entity);
			return true;
		};
		if source.entity != entity {
			let old_sources = self.data_to_drop.entry(params.clone()).or_default();
			old_sources.extend(source.chain.drain(..));
			old_sources.insert(std::mem::replace(&mut source.entity, entity));
			old_sources.remove(&entity);
		}
		let complete = source.chain.is_empty() || source.diffs.as_ref().is_some_and(Vec::is_empty);
		if complete {
			source.diffs = diffs;
		}
		complete
	}
	// The node that applied the diffs becomes the source, the earlier ones are remembered to be dropped with the data.
	fn adopt(&mut self, params: &DT::Params, entity: Entity) -> Vec<DT::Diff> {
		let source = self.sources.get_mut(params).expect("Diffs are only applied to stored data");
		if source.entity != entity {
			source.chain.push(std::mem::replace(&mut source.entity, entity));
		}
		source.diffs.replace(Vec::new()).unwrap_or_default()
	}
	// The stored data that does not satisfy the request and the cheapest node to apply the diffs since it was made.
	fn best_diff(&self, storage: &S, nodes: &DiffApplyingNodes<DT>, param: &SystemParamItem<S::Param>, request: &Request<DT>) -> Option<((f32, Entity), DT::Data)> {
		let (_, stale) = storage.data(param, &request.params).filter(|(dirty_state, _)| !dirty_state.satisfies(&request.allowed_tick))?;
		let diffs = self.sources.get(&request.params)?.diffs.as_ref()?;
		Some((diff::best_node(nodes, request, diffs)?, stale))
	}
}

impl<DT: DataType, S: SimpleFulfillerStorage<DT>> DataPart<DT> for SimpleFulfiller<DT, S> {
	type Storage = S;

	fn setup(app: &mut App) { setup_fulfiller::<DT, Self>(app); }
}

impl<DT: DataType, S: SimpleFulfillerStorage<DT>> Fulfiller<DT> for SimpleFulfiller<DT, S> {
	type Param = (ParamSet<'static, 'static, (DataProducingNodes<'static, 'static, DT>, DiffApplyingNodes<'static, 'static, DT>)>, S::Param);

	fn cost(&self, storage: &S, (nodes, param): &mut SystemParamItem<Self::Param>, request: &Request<DT>) -> Option<f32> {
		let node_cost = best_node(&nodes.p0(), request).map(|(cost, _)| cost);
		let diff_cost = self.best_diff(storage, &nodes.p1(), param, request).map(|((cost, _), _)| cost);
		node_cost.into_iter().chain(diff_cost).reduce(f32::min)
	}
	fn fulfill(&mut self, storage: &mut S, (nodes, param): &mut SystemParamItem<Self::Param>, request: Request<DT>) -> bool {
		let best_node = best_node(&nodes.p0(), &request);
		let best_diff = self.best_diff(storage, &nodes.p1(), param, &request);
		if let Some(((cost, node_entity), mut data)) = best_diff && best_node.is_none_or(|best_node| cost < best_node.0) {
			let diffs = self.adopt(&request.params, node_entity);
			let applied = nodes.p1().get_mut(node_entity).expect("This must exist as it was found in best_diff").0.apply(&request.params, &mut data, diffs);
			// out of date and not worth keeping if it is not applied yet
			storage.store(&request.params, DirtyState::UpToDate, applied.then_some(data));
			return applied;
		}
		if storage.is_requested(&request.params, &request.allowed_tick) { return false; }
		let node_entity = best_node.expect("Data request failed to find node to fulfill the request. This should never happen!").1;
		self.keep(&request.params, node_entity);
		self.requested.entry(request.params.clone()).or_default().insert(node_entity);
		nodes.p0().get_mut(node_entity).expect("This must exist as it was found in best_node").0.request(request);
		false
	}
	fn is_source(&self, params: &DT::Params, source_entity: Entity) -> bool {
		self.requested.get(params).is_some_and(|requested| requested.contains(&source_entity)) || self.uses(params, source_entity)
	}
	fn pass_data(&mut self, storage: &mut S, source_entity: Entity, params: &DT::Params, dirty_state: DirtyState, data: DT::Data) -> bool {
		if self.requested.get_mut(params).is_some_and(|requested| { requested.remove(&source_entity); requested.is_empty() }) {
			self.requested.remove(params);
		}
		let existing = storage.dirty_state(params);
		if existing.is_some_and(|existing| existing.is_fresher_than(&dirty_state)) { return false; }
		if !storage.wanted(params) {
			if !self.sources.get(params).is_some_and(|source| source.entity == source_entity) {
				self.data_to_drop.entry(params.clone()).or_default().insert(source_entity);
			}
			return false; /* No need to store data thats not wanted */
		}
		let complete = self.pass(params, source_entity, dirty_state);
		let dirty_state = existing.filter(|_| !complete).unwrap_or(dirty_state);
		storage.store(params, dirty_state, Some(data));
		true
	}
	fn mark_dirty(&mut self, storage: &mut S, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>) {
		if !storage.mark_dirty(params, last_clean_tick) { return; }
		let Some(source) = self.sources.get_mut(params) else { return; };
		match (&mut source.diffs, diff) {
			(Some(diffs), Some(diff)) => diffs.push(diff.clone()),
			_ => source.diffs = None, // There may never be a dirty inbetween 2 diffs
		}
	}
	fn release(&mut self, _storage: &mut S, params: &DT::Params) {
		let Some(source) = self.sources.remove(params) else { return; };
		self.data_to_drop.entry(params.clone()).or_default().extend(std::iter::once(source.entity).chain(source.chain));
	}
	fn drop_data(&mut self, storage: &S, (nodes, _): &mut SystemParamItem<Self::Param>) {
		if self.data_to_drop.is_empty() { return; }
		let mut nodes = nodes.p0();
		for (params, sources) in std::mem::take(&mut self.data_to_drop) {
			for source_entity in sources {
				if storage.wanted(&params) || self.uses(&params, source_entity) { continue; }
				let Ok((mut node, _)) = nodes.get_mut(source_entity) else { continue; };
				node.drop(&params);
			}
		}
	}
}

// ============================ Simple Storage ============================

/// Keeps the data in a column.
#[derive(Component)]
pub struct SimpleStorage<DT: DataType> {
	data: HashMap<DT::Params, (DirtyState, Option<DT::Data>)>,
	requests: Requests<DT>,
}

impl<DT: DataType> Default for SimpleStorage<DT> {
	fn default() -> Self { Self { data: HashMap::default(), requests: Requests::default() } }
}

impl<DT: DataType> SimpleStorage<DT> {
	fn get(&self, params: &DT::Params) -> Option<(DirtyState, DT::Data)> {
		let (dirty_state, data) = self.data.get(params)?;
		Some((*dirty_state, data.clone()?))
	}
}

impl<DT: DataType> Storage<DT> for SimpleStorage<DT> {}

impl<DT: DataType> SimpleRequesterStorage<DT> for SimpleStorage<DT> {
	type Param = ();

	fn data(&self, _param: &(), params: &DT::Params) -> Option<(DirtyState, DT::Data)> { self.get(params) }
	fn requests(&mut self) -> &mut Requests<DT> { &mut self.requests }
	fn release(&mut self, params: &DT::Params) -> bool { self.data.remove(params).is_some() }
}

impl<DT: DataType> SimpleFulfillerStorage<DT> for SimpleStorage<DT> {
	type Param = ();

	fn data(&self, _param: &(), params: &DT::Params) -> Option<(DirtyState, DT::Data)> { self.get(params) }
	fn dirty_state(&self, params: &DT::Params) -> Option<DirtyState> { self.data.get(params).map(|(dirty_state, _)| *dirty_state) }
	fn wanted(&self, params: &DT::Params) -> bool { self.requests.is_waiting(params) }
	fn is_requested(&self, params: &DT::Params, allowed_tick: &DirtyState) -> bool { self.requests.is_waiting_for(params, allowed_tick) }
	fn store(&mut self, params: &DT::Params, dirty_state: DirtyState, data: Option<DT::Data>) {
		if data.is_some() {
			self.requests.stored(params, dirty_state);
		}
		self.data.insert(params.clone(), (dirty_state, data));
	}
	fn mark_dirty(&mut self, params: &DT::Params, last_clean_tick: u64) -> bool {
		let Some((dirty_state, _)) = self.data.get_mut(params) else { return false; };
		*dirty_state = DirtyState::Dirty { tick: last_clean_tick };
		true
	}
}

// ============================ Registering ============================

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
		self.register_component_as::<dyn DataProducingNode<DT>, C>().init_data_part::<DT, SimpleFulfiller<DT, SimpleStorage<DT>>>()
	}

	fn register_consumer_simple<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + DataConsumingNode<DT>,
	{
		self.register_component_as::<dyn DataConsumingNode<DT>, C>().init_data_part::<DT, SimpleRequester<DT, SimpleStorage<DT>>>()
	}
}
