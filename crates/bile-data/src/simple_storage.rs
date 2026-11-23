use std::collections::{BTreeSet, HashMap};
use std::marker::PhantomData;

use bevy::{ecs::system::SystemParam, prelude::*};
use bevy_trait_query::{One, RegisterExt};

use crate::diff::DiffFulfillerAPI;
use crate::dyn_system_params::{DynParam, RegisterDynParam};
use crate::{btree_drain, unheld_ranges, BileDataSchedule, BileDataSystemSet, Claim, DataType, DirtyState, Request};
use crate::simple_api::{DynFulfillerParams, DynRequesterParams, DynStorageFulfillerParams, DynStorageRequesterParams, FulfillerAPI, RequesterAPI, StorageFulfillerAPI, StorageFulfillerParams, StorageRequesterAPI, StorageRequesterParams};

// ============================ Storage Requests ============================

// The bookkeeping of the requests a storage got, for any storage to use
pub struct StorageRequests<DT: DataType> {
	requests: HashMap<DT::Params, Requests<DT>>,
	applied: Vec<DT::Params>, // The diffs were applied right away, the requesters that wait get the data in the PassData stage
	fell_short: Vec<Request<DT>>, // A diff applier passed data that is too old for them, they are requested again in the Request stage
}

struct Requests<DT: DataType> {
	requesters: HashMap<Entity, Claim>,
	fulfillers: Claim, // What was passed on to the fulfillers
	diffing: BTreeSet<DirtyState>, // What was passed on to a diff applier that passes the data later
	dirties: Option<Vec<(u64, Option<DT::Diff>)>>, // Every dirty the stored data lacks, by tick. None if that is not known
}

impl<DT: DataType> Default for StorageRequests<DT> {
	fn default() -> Self { Self { requests: HashMap::default(), applied: Vec::new(), fell_short: Vec::new() } }
}

impl<DT: DataType> Default for Requests<DT> {
	fn default() -> Self { Self { requesters: HashMap::default(), fulfillers: Claim::default(), diffing: BTreeSet::new(), dirties: None } }
}

impl<DT: DataType> StorageRequests<DT> {
	pub fn is_requested(&self, params: &DT::Params) -> bool { self.requests.contains_key(params) }
	pub fn request(&mut self, requester: Entity, request: &Request<DT>, served: bool) {
		self.requests.entry(request.params.clone()).or_default().requesters.entry(requester).or_default().request(request.allowed_tick, served);
	}
	pub fn is_in_flight(&self, request: &Request<DT>) -> bool {
		self.requests.get(&request.params).is_some_and(|requests| requests.fulfillers.waiting.contains(&request.allowed_tick) || requests.diffing.contains(&request.allowed_tick))
	}
	pub fn is_waited(&self, request: &Request<DT>) -> bool {
		self.requests.get(&request.params).is_some_and(|requests| requests.requesters.values().any(|claim| claim.waiting.contains(&request.allowed_tick)))
	}
	pub fn passed_on(&mut self, request: &Request<DT>, diffing: bool) {
		let requests = self.requests.entry(request.params.clone()).or_default();
		if diffing { requests.diffing.insert(request.allowed_tick); } else { requests.fulfillers.request(request.allowed_tick, false); }
	}
	// The diffs that bring the stored data to a state the request allows, and the tick of the last of them. These are
	// the first dirties, whole ticks up to the first dirty without a diff.
	pub fn diffs(&self, request: &Request<DT>) -> Option<(Vec<DT::Diff>, u64)> {
		let dirties = self.requests.get(&request.params)?.dirties.as_ref()?;
		let blocked = dirties.iter().find(|(_, diff)| diff.is_none()).map(|&(tick, _)| tick);
		let (applicable, rest) = dirties.split_at(dirties.partition_point(|&(tick, _)| blocked.is_none_or(|blocked| tick < blocked)));
		let &(last_tick, _) = applicable.last()?;
		if !state(rest).satisfies(request.allowed_tick) { return None; }
		Some((applicable.iter().map(|(_, diff)| diff.clone().expect("It is before the first dirty without a diff")).collect(), last_tick))
	}
	// The diffs up to the tick were applied right away. Returns the state of the data now.
	pub fn applied(&mut self, params: &DT::Params, last_tick: u64) -> DirtyState {
		let dirties = self.requests.get_mut(params).and_then(|requests| requests.dirties.as_mut()).expect("The diffs came from here");
		dirties.retain(|&(tick, _)| tick > last_tick);
		self.applied.push(params.clone());
		state(dirties)
	}
	pub fn take_applied(&mut self) -> Vec<DT::Params> { std::mem::take(&mut self.applied) }
	pub fn take_fell_short(&mut self) -> Vec<Request<DT>> { std::mem::take(&mut self.fell_short) }
	// A fulfiller passed data, even if it is older than the stored data. Returns the state the data has, which the
	// dirties may show to be newer than it was passed with, or None if the params are not requested.
	pub fn answered(&mut self, params: &DT::Params, dirty_state: DirtyState) -> Option<DirtyState> {
		let requests = self.requests.get_mut(params)?;
		let dirty_state = match (&requests.dirties, dirty_state) {
			// Not if it is older than the stored data, it may lack dirties that are no longer kept
			(Some(dirties), DirtyState::Dirty { tick }) if dirties.first().is_some_and(|&(first, _)| first <= tick) =>
				state(&dirties[dirties.partition_point(|&(dirty_tick, _)| dirty_tick < tick)..]),
			_ => dirty_state,
		};
		requests.fulfillers.pass(dirty_state);
		// A diff applier passes its data once. So what the data is too old for is requested again.
		btree_drain::set_drain_range(&mut requests.diffing, ..=dirty_state);
		self.fell_short.extend(std::mem::take(&mut requests.diffing).into_iter().map(|wanted| Request::new(params.clone(), wanted)));
		Some(dirty_state)
	}
	// New data was stored. Returns the requesters that waited for something it satisfies
	pub fn pass_data(&mut self, params: &DT::Params, dirty_state: DirtyState) -> Vec<Entity> {
		let Some(requests) = self.requests.get_mut(params) else { return Vec::new(); };
		match (&mut requests.dirties, dirty_state) {
			(dirties, DirtyState::UpToDate) => *dirties = Some(Vec::new()),
			(Some(dirties), DirtyState::Dirty { tick }) => dirties.retain(|&(dirty_tick, _)| dirty_tick >= tick),
			(None, _) => {}
		}
		self.satisfied(params, dirty_state)
	}
	// Returns the requesters that waited for something the stored data satisfies
	pub fn satisfied(&mut self, params: &DT::Params, dirty_state: DirtyState) -> Vec<Entity> {
		let Some(requests) = self.requests.get_mut(params) else { return Vec::new(); };
		requests.requesters.iter_mut().filter_map(|(&requester, claim)| claim.pass(dirty_state).then_some(requester)).collect()
	}
	// Returns the requesters that hold the data
	pub fn dirty_data(&mut self, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>) -> Vec<Entity> {
		let Some(requests) = self.requests.get_mut(params) else { return Vec::new(); };
		if let Some(dirties) = &mut requests.dirties {
			dirties.insert(dirties.partition_point(|&(tick, _)| tick <= last_clean_tick), (last_clean_tick, diff.cloned()));
		}
		requests.requesters.keys().copied().collect()
	}
	// Returns what to cancel at the fulfillers. The params are no longer requested if every requester cancelled.
	pub fn cancel(&mut self, requester: Entity, params: &DT::Params, latest: DirtyState, oldest: Option<u64>) -> Vec<(DirtyState, Option<u64>)> {
		let Some(requests) = self.requests.get_mut(params) else { return Vec::new(); };
		let Some(claim) = requests.requesters.get_mut(&requester) else { return Vec::new(); };
		if !claim.cancel(latest, oldest) { return Vec::new(); }
		if claim.is_empty() { requests.requesters.remove(&requester); }
		if requests.requesters.is_empty() {
			let held = !requests.fulfillers.is_empty() || !requests.diffing.is_empty();
			self.requests.remove(params);
			return if held { vec![(DirtyState::UpToDate, None)] } else { Vec::new() };
		}
		// Only what was passed on and nobody waits for anymore. A request served from the storage never reached the
		// fulfillers, so they keep their last state, or they would drop data that is still used.
		let Requests { requesters, fulfillers, diffing, .. } = requests;
		let unwaited = |dirty_state: &DirtyState| dirty_state.is_cancelled_by(latest, oldest) && !requesters.values().any(|claim| claim.waiting.contains(dirty_state));
		let mut cancelled: Vec<DirtyState> = fulfillers.waiting.iter().copied().filter(unwaited).collect();
		if fulfillers.usage.is_none() && cancelled.len() == fulfillers.waiting.len() { cancelled.clear(); }
		cancelled.extend(diffing.iter().copied().filter(unwaited));
		let held = fulfillers.waiting.iter().chain(diffing.iter()).copied().filter(|dirty_state| !cancelled.contains(dirty_state)).chain(fulfillers.usage);
		let mut ranges = unheld_ranges(held, latest, oldest);
		ranges.retain(|&(latest, oldest)| cancelled.iter().any(|dirty_state| dirty_state.is_cancelled_by(latest, oldest)));
		// What can not be cancelled without the usage stays in flight
		let is_cancelled = |dirty_state: &DirtyState| ranges.iter().any(|&(latest, oldest)| dirty_state.is_cancelled_by(latest, oldest));
		fulfillers.waiting.retain(|dirty_state| !is_cancelled(dirty_state));
		diffing.retain(|dirty_state| !is_cancelled(dirty_state));
		ranges
	}
}

// The state of data that lacks the dirties
fn state<D>(dirties: &[(u64, D)]) -> DirtyState {
	dirties.first().map_or(DirtyState::UpToDate, |&(tick, _)| DirtyState::Dirty { tick })
}

// ============================ Fulfillers ============================

pub enum Fulfilling<DT: DataType> {
	Applied(DT::Data), // The diffs were applied right away
	Applying, // A diff applier passes the data later
	Requested, // A fulfiller passes the data later
}

// Reaches the fulfillers of a data type, for any storage to use
#[derive(SystemParam)]
pub struct Fulfillers<'w, 's, DT: DataType> {
	fulfillers: Query<'w, 's, (Entity, One<&'static mut dyn FulfillerAPI<DT>>)>,
	diff_fulfillers: Query<'w, 's, (Entity, One<&'static mut dyn DiffFulfillerAPI<DT>>)>,
	fulfiller_params: DynFulfillerParams<'w, 's, DT>,
}

impl<DT: DataType> Fulfillers<'_, '_, DT> {
	// Hands the request to the cheapest fulfiller. That may be a diff fulfiller if there is stale data and diffs that
	// bring it to a state the request allows, with the tick of the last of them. It returns the data with them applied
	// right away, or passes it later.
	pub fn request(&mut self, request: Request<DT>, stale: Option<(DT::Data, (Vec<DT::Diff>, u64))>) -> Fulfilling<DT> {
		let fulfiller_params = &mut self.fulfiller_params;
		let fulfiller = self.fulfillers.iter()
			.filter_map(|(entity, fulfiller)| Some((fulfiller.cost(fulfiller_params, &request)?, entity)))
			.min_by(|a, b| a.0.total_cmp(&b.0));
		let diff_fulfiller = stale.as_ref().and_then(|(_, (diffs, _))| self.diff_fulfillers.iter()
			.filter_map(|(entity, diff_fulfiller)| Some((diff_fulfiller.cost(fulfiller_params, &request, diffs)?, entity)))
			.min_by(|a, b| a.0.total_cmp(&b.0)));
		if let (Some((cost, entity)), Some((data, (diffs, last_tick)))) = (diff_fulfiller, stale) && fulfiller.is_none_or(|(fulfiller_cost, _)| cost < fulfiller_cost) {
			let applied = self.diff_fulfillers.get_mut(entity).expect("It was just found").1.request(&mut self.fulfiller_params, request, data, diffs, last_tick);
			return applied.map_or(Fulfilling::Applying, Fulfilling::Applied);
		}
		// TODO: a failure path, for now a request nothing can fulfill is a bug
		let Some((_, entity)) = fulfiller else { panic!("No fulfiller of {} can fulfill the request", std::any::type_name::<DT>()); };
		self.fulfillers.get_mut(entity).expect("It was just found").1.request(&mut self.fulfiller_params, request);
		Fulfilling::Requested
	}
	pub fn cancel(&mut self, params: &DT::Params, latest: DirtyState, oldest: Option<u64>) {
		for (_, mut fulfiller) in &mut self.fulfillers {
			fulfiller.cancel(&mut self.fulfiller_params, params, latest, oldest);
		}
		for (_, mut diff_fulfiller) in &mut self.diff_fulfillers {
			diff_fulfiller.cancel(&mut self.fulfiller_params, params, latest, oldest);
		}
	}
}

// ============================ Requesters ============================

// Reaches the requesters of a data type, for any storage to use
#[derive(SystemParam)]
pub struct Requesters<'w, 's, DT: DataType> {
	requesters: Query<'w, 's, One<&'static mut dyn RequesterAPI<DT>>>,
	requester_params: DynRequesterParams<'w, 's, DT>,
}

impl<DT: DataType> Requesters<'_, '_, DT> {
	pub fn pass_data(&mut self, requesters: impl IntoIterator<Item = Entity>, params: &DT::Params, dirty_state: DirtyState, data: &DT::Data) {
		for requester in requesters {
			let Ok(mut requester) = self.requesters.get_mut(requester) else { continue; };
			requester.pass_data(&mut self.requester_params, params, dirty_state, data);
		}
	}
	pub fn dirty_data(&mut self, requesters: impl IntoIterator<Item = Entity>, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>) {
		for requester in requesters {
			let Ok(mut requester) = self.requesters.get_mut(requester) else { continue; };
			requester.dirty_data(&mut self.requester_params, params, last_clean_tick, diff);
		}
	}
}

// ============================ Simple Storage ============================

pub struct StorageRequesterSystemParams<DT: DataType>(PhantomData<DT>);

impl<DT: DataType> DynParam<StorageRequesterParams<DT>> for StorageRequesterSystemParams<DT> {
	type Param = Fulfillers<'static, 'static, DT>;
}

pub struct StorageFulfillerSystemParams<DT: DataType>(PhantomData<DT>);

impl<DT: DataType> DynParam<StorageFulfillerParams<DT>> for StorageFulfillerSystemParams<DT> {
	type Param = Requesters<'static, 'static, DT>;
}

#[derive(Resource)]
pub struct Storage<DT: DataType> {
	data: HashMap<DT::Params, (DirtyState, Option<DT::Data>)>,
	requests: StorageRequests<DT>,
}

impl<DT: DataType> Default for Storage<DT> {
	fn default() -> Self { Self { data: HashMap::default(), requests: StorageRequests::default() } }
}

impl<DT: DataType> Storage<DT> {
	pub fn get(&self, params: &DT::Params) -> Option<(DirtyState, DT::Data)> {
		let (dirty_state, data) = self.data.get(params)?;
		Some((*dirty_state, data.clone()?))
	}
	// Passes the request on to the fulfillers. Returns the data if diffs were applied to the stored data right away.
	fn fulfill(&mut self, storage_params: &mut DynStorageRequesterParams<DT>, request: &Request<DT>) -> Option<(DT::Data, DirtyState)> {
		let stored = self.data.get(&request.params).and_then(|(_, data)| data.as_ref());
		let stale = stored.and_then(|data| Some((data.clone(), self.requests.diffs(request)?)));
		let last_tick = stale.as_ref().map(|(_, (_, last_tick))| *last_tick);
		let data = match storage_params.get::<StorageRequesterSystemParams<DT>>().request(request.clone(), stale) {
			Fulfilling::Applied(data) => data,
			fulfilling => {
				self.requests.passed_on(request, matches!(fulfilling, Fulfilling::Applying));
				return None;
			}
		};
		let dirty_state = self.requests.applied(&request.params, last_tick.expect("Diffs were applied"));
		self.data.insert(request.params.clone(), (dirty_state, Some(data.clone())));
		Some((data, dirty_state))
	}
}

impl<DT: DataType> StorageRequesterAPI<DT> for Storage<DT> {
	fn request(
		&mut self,
		storage_params: &mut DynStorageRequesterParams<DT>,
		requester: Entity,
		request: &Request<DT>,
	) -> Option<(DT::Data, DirtyState)>  {
		let stored = self.data.get(&request.params).and_then(|(dirty_state, data)| Some((*dirty_state, data.as_ref()?)));
		if let Some((dirty_state, data)) = stored && dirty_state >= request.allowed_tick {
			self.requests.request(requester, request, true);
			return Some((data.clone(), dirty_state));
		}
		if self.requests.is_in_flight(request) {
			self.requests.request(requester, request, false);
			return None;
		}
		let served = self.fulfill(storage_params, request);
		self.requests.request(requester, request, served.is_some());
		served
	}

	fn cancel(
		&mut self,
		storage_params: &mut DynStorageRequesterParams<DT>,
		requester: Entity,
		params: &DT::Params,
		latest: DirtyState,
		oldest: Option<u64>, // None if everything before latest
	) {
		let cancel = self.requests.cancel(requester, params, latest, oldest);
		if !self.requests.is_requested(params) { self.data.remove(params); }
		if cancel.is_empty() { return; }
		let mut fulfillers = storage_params.get::<StorageRequesterSystemParams<DT>>();
		for (latest, oldest) in cancel {
			fulfillers.cancel(params, latest, oldest);
		}
	}
}

impl<DT: DataType> StorageFulfillerAPI<DT> for Storage<DT> {
	fn pass_data(
		&mut self,
		storage_params: &mut DynStorageFulfillerParams<DT>,
		params: &DT::Params,
		dirty_state: DirtyState,
		data: DT::Data,
	) {
		let Some(dirty_state) = self.requests.answered(params, dirty_state) else { return; };
		if self.data.get(params).is_some_and(|(stored, _)| *stored > dirty_state) { return; }
		let requesters = self.requests.pass_data(params, dirty_state);
		storage_params.get::<StorageFulfillerSystemParams<DT>>().pass_data(requesters, params, dirty_state, &data);
		self.data.insert(params.clone(), (dirty_state, Some(data)));
	}

	fn dirty_data(
		&mut self,
		storage_params: &mut DynStorageFulfillerParams<DT>,
		params: &DT::Params,
		last_clean_tick: u64,
		diff: Option<&DT::Diff>,
	) {
		if let Some((dirty_state, _)) = self.data.get_mut(params) {
			*dirty_state = (*dirty_state).min(DirtyState::Dirty { tick: last_clean_tick });
		}
		let requesters = self.requests.dirty_data(params, last_clean_tick, diff);
		storage_params.get::<StorageFulfillerSystemParams<DT>>().dirty_data(requesters, params, last_clean_tick, diff);
	}
}

fn pass_applied<DT: DataType>(mut storage: ResMut<Storage<DT>>, mut requesters: Requesters<DT>) {
	let storage = &mut *storage;
	for params in storage.requests.take_applied() {
		let Some((dirty_state, Some(data))) = storage.data.get(&params) else { continue; };
		requesters.pass_data(storage.requests.satisfied(&params, *dirty_state), &params, *dirty_state, data);
	}
}

// What the data of a diff applier was too old for
fn request_again<DT: DataType>(mut storage: ResMut<Storage<DT>>, mut storage_params: DynStorageRequesterParams<DT>) {
	for request in storage.requests.take_fell_short() {
		// Not if the stored data is enough by now, the requesters that wait get that in the PassData stage
		if storage.data.get(&request.params).is_some_and(|(stored, _)| *stored >= request.allowed_tick) { continue; }
		if storage.requests.is_waited(&request) && !storage.requests.is_in_flight(&request) { storage.fulfill(&mut storage_params, &request); }
	}
}

pub trait BileAddSimpleStorage {
	fn add_simple_storage<DT: DataType>(&mut self) -> &mut Self;
}

impl BileAddSimpleStorage for App {
	fn add_simple_storage<DT: DataType>(&mut self) -> &mut Self {
		self.init_resource::<Storage<DT>>()
			.register_component_as::<dyn StorageRequesterAPI<DT>, Storage<DT>>()
			.register_component_as::<dyn StorageFulfillerAPI<DT>, Storage<DT>>()
			.register_dyn_param::<StorageRequesterParams<DT>, StorageRequesterSystemParams<DT>>()
			.register_dyn_param::<StorageFulfillerParams<DT>, StorageFulfillerSystemParams<DT>>()
			.add_systems(BileDataSchedule, (request_again::<DT>.in_set(BileDataSystemSet::Request), pass_applied::<DT>.in_set(BileDataSystemSet::PassData)))
	}
}
