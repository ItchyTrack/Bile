use std::{
	any::type_name,
	collections::{HashMap, HashSet},
	marker::PhantomData,
};

use bevy::{
	ecs::system::{StaticSystemParam, SystemParam, SystemParamItem},
	prelude::*,
};
use bevy_trait_query::{One, RegisterExt};

use crate::{
	diff::{self, DiffApplyingNodes},
	dyn_system_params::{DynParam, DynSystemParams, RegisterDynParam},
	parts::{BileSetDataParts, DataParts, Part, SetupParts},
	setup::{BileDataSchedule, BileDataSystemSet, DataType, DirtyState, Request, StateTick},
};

// ============================ Nodes ============================

#[bevy_trait_query::queryable]
pub trait DataConsumingNode<DT: DataType> {
	fn pass_data(&mut self, params: &DT::Params, tick: u64, data: DT::Data);
	fn dirty_data(&mut self, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>);
}

#[bevy_trait_query::queryable]
pub trait DataProducingNode<DT: DataType> {
	fn cost(&self, request: &Request<DT>) -> Option<f32>; // None if the node can not fulfill the request
	fn request(&mut self, request: Request<DT>);
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
	parts: Query<'w, 's, One<&'static mut dyn SimpleParts<DT>>>,
	system_params: DynSystemParams<'w, 's, Requesting<DT>>,
}

impl<DT: DataType> DataRequester<'_, '_, DT> {
	pub fn request(&mut self, request: Request<DT>, requester_node: Entity) -> Option<DT::Data> {
		let Ok(mut parts) = self.parts.single_mut() else { panic!("{} has no data parts, a node of it has to be registered.", type_name::<DT>()) };
		parts.request(&mut self.system_params, request, requester_node)
	}
	pub fn cancel(&mut self, params: &DT::Params, requester_node: Entity) {
		if let Ok(mut parts) = self.parts.single_mut() {
			parts.cancel(params, requester_node);
		}
	}
}

#[derive(SystemParam)]
pub struct DataPasser<'w, 's, DT: DataType> {
	parts: Query<'w, 's, One<&'static mut dyn SimpleParts<DT>>>,
}

impl<DT: DataType> DataPasser<'_, '_, DT> {
	pub fn pass_data(&mut self, source_entity: Entity, params: &DT::Params, dirty_state: DirtyState, data: DT::Data) {
		if let Ok(mut parts) = self.parts.single_mut() {
			parts.pass_data(source_entity, params, dirty_state, data);
		}
	}
}

#[derive(SystemParam)]
pub struct DataDirtier<'w, 's, DT: DataType> {
	bile_state_tick: Res<'w, StateTick>,
	parts: Query<'w, 's, One<&'static mut dyn SimpleParts<DT>>>,
	system_params: DynSystemParams<'w, 's, Dirtying<DT>>,
}

impl<DT: DataType> DataDirtier<'_, '_, DT> {
	pub fn mark_dirty(&mut self, params: &DT::Params, diff: Option<&DT::Diff>) {
		let last_clean_tick = self.bile_state_tick.update(); /* Was valid on the last tick */
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
pub trait SimpleParts<DT: DataType> {
	fn request(&mut self, system_params: &mut DynSystemParams<Requesting<DT>>, request: Request<DT>, requester_node: Entity) -> Option<DT::Data>;
	fn cancel(&mut self, params: &DT::Params, requester_node: Entity);
	fn pass_data(&mut self, source_entity: Entity, params: &DT::Params, dirty_state: DirtyState, data: DT::Data);
	fn mark_dirty(&mut self, system_params: &mut DynSystemParams<Dirtying<DT>>, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>);
}

// ============================ Parts ============================

/// Brings the data to the nodes that request it, from the storage `S` or else by having the fulfiller `F` get it.
pub trait Requester<DT: DataType, S, F: Fulfiller<DT, S>>: Part {
	type RequestParam: SystemParam + 'static; // to look in the storage
	type NodesParam: SystemParam + 'static;   // to reach the consuming nodes

	/// Returns the data if the storage has it or the fulfiller could get it right away. Otherwise it is passed later.
	fn request(
		&mut self,
		storage: &mut S,
		param: &mut SystemParamItem<'_, '_, Self::RequestParam>,
		fulfiller: &mut F,
		fulfiller_param: &mut SystemParamItem<'_, '_, F::Param>,
		request: Request<DT>,
		requester_node: Entity,
	) -> Option<DT::Data>;
	fn cancel(&mut self, storage: &mut S, params: &DT::Params, requester_node: Entity);
	/// The fulfiller stored new data.
	fn stored(&mut self, storage: &mut S, params: &DT::Params, dirty_state: DirtyState);
	fn mark_dirty(&mut self, storage: &mut S, nodes: &mut SystemParamItem<'_, '_, Self::NodesParam>, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>);
	/// Passes the stored data to the nodes waiting for it.
	fn pass_data(&mut self, storage: &mut S, param: &mut SystemParamItem<'_, '_, Self::RequestParam>, nodes: &mut SystemParamItem<'_, '_, Self::NodesParam>, current_tick: u64);
}

/// Gets the data into the storage `S` from the producing nodes.
pub trait Fulfiller<DT: DataType, S>: Part {
	type Param: SystemParam + 'static; // to reach the producing nodes

	/// Brings the data up to date right away if it can and returns true, or else has a node produce it.
	fn fulfill(&mut self, storage: &mut S, param: &mut SystemParamItem<'_, '_, Self::Param>, request: Request<DT>) -> bool;
	/// A node passed data. Returns how fresh it is if it was stored.
	fn pass_data(&mut self, storage: &mut S, source_entity: Entity, params: &DT::Params, dirty_state: DirtyState, data: DT::Data) -> Option<DirtyState>;
	fn mark_dirty(&mut self, storage: &mut S, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>);
	/// Drops the nodes the stored data no longer comes from.
	fn drop_data(&mut self, storage: &mut S, param: &mut SystemParamItem<'_, '_, Self::Param>);
}

impl<DT, R, S, F> SimpleParts<DT> for DataParts<DT, R, S, F>
where DT: DataType, R: Requester<DT, S, F>, S: Part, F: Fulfiller<DT, S>,
{
	fn request(&mut self, system_params: &mut DynSystemParams<Requesting<DT>>, request: Request<DT>, requester_node: Entity) -> Option<DT::Data> {
		let (param, fulfiller_param) = &mut system_params.get::<Self>();
		self.requester.request(&mut self.storage, param, &mut self.fulfiller, fulfiller_param, request, requester_node)
	}
	fn cancel(&mut self, params: &DT::Params, requester_node: Entity) { self.requester.cancel(&mut self.storage, params, requester_node); }
	fn pass_data(&mut self, source_entity: Entity, params: &DT::Params, dirty_state: DirtyState, data: DT::Data) {
		if let Some(dirty_state) = self.fulfiller.pass_data(&mut self.storage, source_entity, params, dirty_state, data) {
			self.requester.stored(&mut self.storage, params, dirty_state);
		}
	}
	fn mark_dirty(&mut self, system_params: &mut DynSystemParams<Dirtying<DT>>, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>) {
		self.fulfiller.mark_dirty(&mut self.storage, params, last_clean_tick, diff);
		self.requester.mark_dirty(&mut self.storage, &mut system_params.get::<Self>(), params, last_clean_tick, diff);
	}
}

impl<DT, R, S, F> DynParam<Requesting<DT>> for DataParts<DT, R, S, F>
where DT: DataType, R: Requester<DT, S, F>, S: Part, F: Fulfiller<DT, S>,
{
	type Param = (R::RequestParam, F::Param);
}

impl<DT, R, S, F> DynParam<Dirtying<DT>> for DataParts<DT, R, S, F>
where DT: DataType, R: Requester<DT, S, F>, S: Part, F: Fulfiller<DT, S>,
{
	type Param = R::NodesParam;
}

fn setup<DT, R, S, F>(app: &mut App)
where DT: DataType, R: Requester<DT, S, F>, S: Part, F: Fulfiller<DT, S>,
{
	app.register_component_as::<dyn SimpleParts<DT>, DataParts<DT, R, S, F>>()
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

fn pass_data<DT, R, S, F>(
	bile_state_tick: Res<StateTick>,
	mut parts: Query<&mut DataParts<DT, R, S, F>>,
	mut param: StaticSystemParam<R::RequestParam>,
	mut nodes: StaticSystemParam<R::NodesParam>,
) where DT: DataType, R: Requester<DT, S, F>, S: Part, F: Fulfiller<DT, S>,
{
	for mut parts in &mut parts {
		let DataParts { requester, storage, .. } = &mut *parts;
		requester.pass_data(storage, &mut param, &mut nodes, bile_state_tick.current_tick());
	}
}

// ============================ Simple Requester ============================

/// What the simple requester needs from a storage.
pub trait SimpleRequesterStorage<DT: DataType>: Part {
	type Param: SystemParam + 'static; // to read the data

	fn data(&self, param: &SystemParamItem<Self::Param>, params: &DT::Params) -> Option<(DirtyState, DT::Data)>;
	fn requests(&mut self) -> &mut Requests<DT>;
	/// No node uses or waits for the data anymore.
	fn release(&mut self, params: &DT::Params);
}

/// What the simple requester keeps in the storage: the nodes that requested the data.
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

/// Passes single pieces of data to the nodes that request them.
#[derive(Default)]
pub struct SimpleRequester;

impl SimpleRequester {
	fn stored_data<DT: DataType, S: SimpleRequesterStorage<DT>>(storage: &S, param: &SystemParamItem<S::Param>, request: &Request<DT>) -> Option<DT::Data> {
		storage.data(param, &request.params).filter(|(dirty_state, _)| dirty_state.satisfies(&request.allowed_tick)).map(|(_, data)| data)
	}
}

impl<DT, S, F> Requester<DT, S, F> for SimpleRequester
where DT: DataType, S: SimpleRequesterStorage<DT>, F: Fulfiller<DT, S>,
{
	type RequestParam = S::Param;
	type NodesParam = DataConsumingNodes<'static, 'static, DT>;

	fn request(
		&mut self,
		storage: &mut S,
		param: &mut SystemParamItem<S::Param>,
		fulfiller: &mut F,
		fulfiller_param: &mut SystemParamItem<F::Param>,
		request: Request<DT>,
		requester_node: Entity,
	) -> Option<DT::Data> {
		let mut data = Self::stored_data(storage, param, &request);
		if data.is_none() {
			let (params, allowed_tick) = (request.params.clone(), request.allowed_tick);
			if fulfiller.fulfill(storage, fulfiller_param, Request::new(params.clone(), allowed_tick)) {
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
	fn cancel(&mut self, storage: &mut S, params: &DT::Params, requester_node: Entity) {
		let requests = storage.requests();
		requests.forget(params, requester_node);
		if !requests.is_requested(params) {
			storage.release(params);
		}
	}
	fn stored(&mut self, storage: &mut S, params: &DT::Params, dirty_state: DirtyState) {
		let requests = storage.requests();
		let Some(waiting) = requests.waiting.remove(params) else { return; };
		let (satisfied, still_waiting): (HashMap<_, _>, HashMap<_, _>) = waiting.into_iter().partition(|(_, required)| dirty_state.satisfies(required));
		if !satisfied.is_empty() {
			requests.finished.entry(params.clone()).or_default().extend(satisfied.into_keys());
		}
		if !still_waiting.is_empty() {
			requests.waiting.insert(params.clone(), still_waiting);
		}
	}
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
	fn pass_data(&mut self, storage: &mut S, param: &mut SystemParamItem<S::Param>, nodes: &mut DataConsumingNodes<DT>, current_tick: u64) {
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

impl<DT, S, F> SetupParts<DT, S, F> for SimpleRequester
where DT: DataType, S: SimpleRequesterStorage<DT>, F: Fulfiller<DT, S>,
{
	fn setup(app: &mut App) { setup::<DT, Self, S, F>(app); }
}

// ============================ Simple Fulfiller ============================

/// What the simple fulfiller needs from a storage.
pub trait SimpleFulfillerStorage<DT: DataType>: Part {
	type Param: SystemParam + 'static; // to read the data

	fn data(&self, param: &SystemParamItem<Self::Param>, params: &DT::Params) -> Option<(DirtyState, DT::Data)>;
	fn dirty_state(&self, params: &DT::Params) -> Option<DirtyState>; // None if nothing is stored
	fn sources(&mut self) -> &mut Sources<DT>;
	/// Something waits for the data, so it is stored when it is passed.
	fn wanted(&self, params: &DT::Params) -> bool;
	/// Data that satisfies `allowed_tick` is already on its way.
	fn is_requested(&self, params: &DT::Params, allowed_tick: &DirtyState) -> bool;
	/// None if the data is out of date and not worth keeping.
	fn store(&mut self, params: &DT::Params, dirty_state: DirtyState, data: Option<DT::Data>);
	/// Returns false if nothing is stored.
	fn mark_dirty(&mut self, params: &DT::Params, last_clean_tick: u64) -> bool;
}

/// What the simple fulfiller keeps in the storage: the nodes the data comes from.
pub struct Sources<DT: DataType> {
	sources: HashMap<DT::Params, Source<DT>>,
	data_to_drop: HashMap<DT::Params, HashSet<Entity>>, // this data is no longer requested from these. Either no one wants it or something else provided it.
}

struct Source<DT: DataType> {
	entity: Entity,
	chain: Vec<Entity>,           // earlier sources, whose out of date data is not stored
	diffs: Option<Vec<DT::Diff>>, // every dirty since the data was made. None if one had no diff
}

impl<DT: DataType> Default for Sources<DT> {
	fn default() -> Self { Self { sources: HashMap::default(), data_to_drop: HashMap::default() } }
}

impl<DT: DataType> Sources<DT> {
	/// The data is no longer stored, so the nodes it came from are dropped.
	pub fn release(&mut self, params: &DT::Params) {
		let Some(source) = self.sources.remove(params) else { return; };
		self.data_to_drop.entry(params.clone()).or_default().extend(std::iter::once(source.entity).chain(source.chain));
	}
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
	fn mark_dirty(&mut self, params: &DT::Params, diff: Option<&DT::Diff>) {
		let Some(source) = self.sources.get_mut(params) else { return; };
		match (&mut source.diffs, diff) {
			(Some(diffs), Some(diff)) => diffs.push(diff.clone()),
			_ => source.diffs = None, // There may never be a dirty inbetween 2 diffs
		}
	}
}

/// Gets single pieces of data from the cheapest node, or applies the diffs since it was made if that is cheaper.
#[derive(Default)]
pub struct SimpleFulfiller;

impl<DT: DataType, S: SimpleFulfillerStorage<DT>> Fulfiller<DT, S> for SimpleFulfiller {
	type Param = (ParamSet<'static, 'static, (DataProducingNodes<'static, 'static, DT>, DiffApplyingNodes<'static, 'static, DT>)>, S::Param);

	fn fulfill(&mut self, storage: &mut S, (nodes, param): &mut SystemParamItem<Self::Param>, request: Request<DT>) -> bool {
		let best_node = best_node(&nodes.p0(), &request);
		let stale = storage.data(param, &request.params).filter(|(dirty_state, _)| !dirty_state.satisfies(&request.allowed_tick)).map(|(_, data)| data);
		let best_diff = stale.and_then(|data| {
			let diffs = storage.sources().sources.get(&request.params)?.diffs.as_ref()?;
			Some((diff::best_node(&nodes.p1(), &request, diffs)?, data))
		});
		if let Some(((cost, node_entity), mut data)) = best_diff && best_node.is_none_or(|best_node| cost < best_node.0) {
			let diffs = storage.sources().adopt(&request.params, node_entity);
			let applied = nodes.p1().get_mut(node_entity).expect("This must exist as it was found in best_diff").0.apply(&request.params, &mut data, diffs);
			// out of date and not worth keeping if it is not applied yet
			storage.store(&request.params, DirtyState::UpToDate, applied.then_some(data));
			return applied;
		}
		if storage.is_requested(&request.params, &request.allowed_tick) { return false; }
		let node_entity = best_node.expect("Data request failed to find node to fulfill the request. This should never happen!").1;
		storage.sources().keep(&request.params, node_entity);
		nodes.p0().get_mut(node_entity).expect("This must exist as it was found in best_node").0.request(request);
		false
	}
	fn pass_data(&mut self, storage: &mut S, source_entity: Entity, params: &DT::Params, dirty_state: DirtyState, data: DT::Data) -> Option<DirtyState> {
		let existing = storage.dirty_state(params);
		if existing.is_some_and(|existing| existing.is_fresher_than(&dirty_state)) { return None; }
		if !storage.wanted(params) {
			if !storage.sources().sources.get(params).is_some_and(|source| source.entity == source_entity) {
				storage.sources().data_to_drop.entry(params.clone()).or_default().insert(source_entity);
			}
			return None; /* No need to store data thats not wanted */
		}
		let complete = storage.sources().pass(params, source_entity, dirty_state);
		let dirty_state = existing.filter(|_| !complete).unwrap_or(dirty_state);
		storage.store(params, dirty_state, Some(data));
		Some(dirty_state)
	}
	fn mark_dirty(&mut self, storage: &mut S, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>) {
		if storage.mark_dirty(params, last_clean_tick) {
			storage.sources().mark_dirty(params, diff);
		}
	}
	fn drop_data(&mut self, storage: &mut S, (nodes, _): &mut SystemParamItem<Self::Param>) {
		if storage.sources().data_to_drop.is_empty() { return; }
		let mut nodes = nodes.p0();
		for (params, sources) in std::mem::take(&mut storage.sources().data_to_drop) {
			for source_entity in sources {
				if storage.wanted(&params) || storage.sources().uses(&params, source_entity) { continue; }
				let Ok((mut node, _)) = nodes.get_mut(source_entity) else { continue; };
				node.drop(&params);
			}
		}
	}
}

// ============================ Simple Storage ============================

/// Keeps the data in a column.
pub struct SimpleStorage<DT: DataType> {
	data: HashMap<DT::Params, (DirtyState, Option<DT::Data>)>,
	requests: Requests<DT>,
	sources: Sources<DT>,
}

impl<DT: DataType> Default for SimpleStorage<DT> {
	fn default() -> Self { Self { data: HashMap::default(), requests: Requests::default(), sources: Sources::default() } }
}

impl<DT: DataType> SimpleStorage<DT> {
	fn get(&self, params: &DT::Params) -> Option<(DirtyState, DT::Data)> {
		let (dirty_state, data) = self.data.get(params)?;
		Some((*dirty_state, data.clone()?))
	}
}

impl<DT: DataType> SimpleRequesterStorage<DT> for SimpleStorage<DT> {
	type Param = ();

	fn data(&self, _param: &(), params: &DT::Params) -> Option<(DirtyState, DT::Data)> { self.get(params) }
	fn requests(&mut self) -> &mut Requests<DT> { &mut self.requests }
	fn release(&mut self, params: &DT::Params) {
		if self.data.remove(params).is_some() {
			self.sources.release(params);
		}
	}
}

impl<DT: DataType> SimpleFulfillerStorage<DT> for SimpleStorage<DT> {
	type Param = ();

	fn data(&self, _param: &(), params: &DT::Params) -> Option<(DirtyState, DT::Data)> { self.get(params) }
	fn dirty_state(&self, params: &DT::Params) -> Option<DirtyState> { self.data.get(params).map(|(dirty_state, _)| *dirty_state) }
	fn sources(&mut self) -> &mut Sources<DT> { &mut self.sources }
	fn wanted(&self, params: &DT::Params) -> bool { self.requests.is_waiting(params) }
	fn is_requested(&self, params: &DT::Params, allowed_tick: &DirtyState) -> bool { self.requests.is_waiting_for(params, allowed_tick) }
	fn store(&mut self, params: &DT::Params, dirty_state: DirtyState, data: Option<DT::Data>) { self.data.insert(params.clone(), (dirty_state, data)); }
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
		self.register_component_as::<dyn DataProducingNode<DT>, C>().init_data_parts::<DT, SimpleRequester, SimpleStorage<DT>, SimpleFulfiller>()
	}

	fn register_consumer_simple<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + DataConsumingNode<DT>,
	{
		self.register_component_as::<dyn DataConsumingNode<DT>, C>().init_data_parts::<DT, SimpleRequester, SimpleStorage<DT>, SimpleFulfiller>()
	}
}
