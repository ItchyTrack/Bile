use std::collections::{BTreeSet, HashMap};
use std::marker::PhantomData;

use bevy::{ecs::system::SystemParam, prelude::*};
use bevy_trait_query::{One, RegisterExt};

use crate::diff::DiffFulfillerAPI;
use crate::dyn_system_params::{DynParam, RegisterDynParam};
use crate::{btree_drain, cancel_range::{cancel_range, is_cancelled, unheld_ranges}, BileDataSchedule, BileDataSystemSet, DataType, DirtyState, Request};
use crate::simple_api::{DynFulfillerParams, DynRequesterParams, DynStorageFulfillerParams, DynStorageRequesterParams, FulfillerAPI, RequesterAPI, StorageFulfillerAPI, StorageFulfillerParams, StorageRequesterAPI, StorageRequesterParams};

// ============================ Storage Requests ============================

// The bookkeeping of the requests a storage got, for any storage to use
pub struct StorageRequests<DT: DataType> {
	requests: HashMap<DT::Params, Requests<DT>>,
	applied: Vec<DT::Params>, // The diffs were applied right away, the requesters that wait get the data in the PassData stage
	fell_short: Vec<Request<DT>>, // What the data of a diff applier is too old for, they are requested again in the Request stage
}

struct Requests<DT: DataType> {
	requesters: HashMap<Entity, (BTreeSet<DirtyState>, Option<DirtyState>)>, // Requester -> The dirty states it waits for, and the wanted dirty state it uses
	fulfillers: HashMap<Entity, (BTreeSet<DirtyState>, Option<DirtyState>)>, // Fulfiller -> What was passed on to it that no data of it satisfied yet, and the freshest of that its last data satisfied. It keeps that.
	diffing: Option<(Entity, Option<DirtyState>, Vec<(u64, Option<DT::Diff>)>)>, // The diff fulfiller that passes the data later, what it was asked for (None if newer data replaced what it works on), and the dirties it was given
	dirties: Option<Vec<(u64, Option<DT::Diff>)>>, // Every other dirty the stored data lacks, by tick. None if that is not known
	source: Option<Entity>, // The fulfiller the stored data came from. It keeps something, or its nodes would drop data that is still used.
	has_below: u64, // The stored data was passed as having every dirty below this tick
	applied_through: Option<u64>, // The tick of the last diff that was applied to the stored data, an older diff can not be applied after it
}

impl<DT: DataType> Default for StorageRequests<DT> {
	fn default() -> Self { Self { requests: HashMap::default(), applied: Vec::new(), fell_short: Vec::new() } }
}

impl<DT: DataType> Default for Requests<DT> {
	fn default() -> Self { Self { requesters: HashMap::default(), fulfillers: HashMap::default(), diffing: None, dirties: None, source: None, has_below: 0, applied_through: None } }
}

impl<DT: DataType> StorageRequests<DT> {
	pub fn is_requested(&self, params: &DT::Params) -> bool { self.requests.contains_key(params) }
	pub fn request(&mut self, requester: Entity, request: &Request<DT>, served: bool) {
		let (waiting, usage) = self.requests.entry(request.params.clone()).or_default().requesters.entry(requester).or_default();
		if served { *usage = Some(request.allowed_tick); } else { waiting.insert(request.allowed_tick); }
	}
	pub fn is_in_flight(&self, request: &Request<DT>) -> bool {
		self.requests.get(&request.params).is_some_and(|requests| requests.fulfillers.values().any(|(passed_on, _)| passed_on.contains(&request.allowed_tick))
			|| requests.diffing.as_ref().is_some_and(|&(_, wanted, _)| wanted == Some(request.allowed_tick)))
	}
	pub fn is_waited(&self, request: &Request<DT>) -> bool {
		self.requests.get(&request.params).is_some_and(|requests| requests.requesters.values().any(|(waiting, _)| waiting.contains(&request.allowed_tick)))
	}
	pub fn passed_on(&mut self, request: &Request<DT>, fulfiller: Entity) {
		self.requests.entry(request.params.clone()).or_default().fulfillers.entry(fulfiller).or_default().0.insert(request.allowed_tick);
	}
	// The first diffs were given to a diff applier that passes the data later
	pub fn diffing(&mut self, request: &Request<DT>, diff_fulfiller: Entity, diffs: usize) {
		let requests = self.requests.get_mut(&request.params).expect("The diffs came from here");
		let handed = requests.dirties.as_mut().expect("The diffs came from here").drain(..diffs).collect();
		requests.diffing = Some((diff_fulfiller, Some(request.allowed_tick), handed));
	}
	// The diffs that bring the stored data to a state the request allows. These are the first dirties, whole ticks up to
	// the first dirty without a diff. None while a diff applier works on the data, one is enough.
	pub fn diffs(&self, request: &Request<DT>) -> Option<Vec<DT::Diff>> {
		let dirties = self.requests.get(&request.params).filter(|requests| requests.diffing.is_none())?.dirties.as_ref()?;
		let blocked = dirties.iter().find(|(_, diff)| diff.is_none()).map(|&(tick, _)| tick);
		let (applicable, rest) = dirties.split_at(dirties.partition_point(|&(tick, _)| blocked.is_none_or(|blocked| tick < blocked)));
		if applicable.is_empty() || !state(rest).satisfies(request.allowed_tick) { return None; }
		Some(applicable.iter().map(|(_, diff)| diff.clone().expect("It is before the first dirty without a diff")).collect())
	}
	// The first diffs were applied right away. Returns the state of the data now.
	pub fn applied(&mut self, params: &DT::Params, diffs: usize) -> DirtyState {
		let requests = self.requests.get_mut(params).expect("The diffs came from here");
		let dirties = requests.dirties.as_mut().expect("The diffs came from here");
		requests.applied_through = dirties.drain(..diffs).last().map(|(tick, _)| tick);
		self.applied.push(params.clone());
		state(dirties)
	}
	pub fn take_applied(&mut self) -> Vec<DT::Params> { std::mem::take(&mut self.applied) }
	pub fn take_fell_short(&mut self) -> Vec<Request<DT>> { std::mem::take(&mut self.fell_short) }
	// A fulfiller passed data, even if it is older than the stored data. Returns the state the data has, or None if it is
	// not to be stored: the params are not requested, the fulfiller was not asked, or newer data replaced what a diff
	// applier worked on.
	pub fn answered(&mut self, fulfiller: Entity, params: &DT::Params, dirty_state: DirtyState) -> Option<DirtyState> {
		let requests = self.requests.get_mut(params)?;
		if requests.diffing.as_ref().is_some_and(|&(diff_fulfiller, ..)| diff_fulfiller == fulfiller) {
			// It lacks the dirties it was not given, the state it was passed with means nothing
			let (_, wanted, handed) = requests.diffing.take().expect("It was just found");
			let dirties = requests.dirties.as_mut().expect("The diffs came from here");
			// An older diff that came meanwhile can not be applied after the ones it was given
			if let Some(&(last, _)) = handed.last() {
				requests.applied_through = Some(last);
				for (_, diff) in dirties.iter_mut().filter(|(tick, _)| *tick <= last) { *diff = None; }
			}
			let dirty_state = state(dirties);
			// A diff applier passes its data once. So what the data is too old for is requested again.
			if let Some(wanted) = wanted && !dirty_state.satisfies(wanted) { self.fell_short.push(Request::new(params.clone(), wanted)); }
			return wanted.and(Some(dirty_state));
		}
		let (passed_on, used) = requests.fulfillers.get_mut(&fulfiller)?;
		if let Some(&wanted) = btree_drain::set_drain_range(passed_on, ..=dirty_state).last() { *used = Some(wanted); }
		Some(dirty_state)
	}
	// New data of the fulfiller was stored. Returns the requesters that waited for something it satisfies
	pub fn pass_data(&mut self, fulfiller: Entity, params: &DT::Params, dirty_state: DirtyState) -> Vec<Entity> {
		let Some(requests) = self.requests.get_mut(params) else { return Vec::new(); };
		// Not for what a diff applier passed, that is still the data of the fulfiller it came from
		if requests.fulfillers.contains_key(&fulfiller) {
			requests.source = Some(fulfiller);
			requests.has_below = match dirty_state { DirtyState::Dirty { tick } => tick, DirtyState::UpToDate => 0 };
			requests.applied_through = None;
		}
		// A diff applier works on older data, what it passes is dropped and what this is not enough for is requested again
		if let Some((_, wanted, handed)) = &mut requests.diffing && let Some(wanted) = wanted.take() {
			self.fell_short.push(Request::new(params.clone(), wanted));
			put_back(requests.dirties.as_mut().expect("The diffs came from here"), std::mem::take(handed));
		}
		match (&mut requests.dirties, dirty_state) {
			(dirties, DirtyState::UpToDate) => *dirties = Some(Vec::new()),
			(Some(dirties), DirtyState::Dirty { tick }) => {
				dirties.retain(|&(dirty_tick, _)| dirty_tick >= tick);
				// It lacks a dirty that did not arrive here, no diff is applied past that
				if dirties.first().is_none_or(|&(first, _)| first > tick) { dirties.insert(0, (tick, None)); }
			}
			(None, _) => {}
		}
		self.satisfied(params, dirty_state)
	}
	// Returns the requesters that waited for something the stored data satisfies
	pub fn satisfied(&mut self, params: &DT::Params, dirty_state: DirtyState) -> Vec<Entity> {
		let Some(requests) = self.requests.get_mut(params) else { return Vec::new(); };
		// The freshest request of a requester the data satisfies is what it uses
		requests.requesters.iter_mut().filter_map(|(&requester, (waiting, usage))| {
			*usage = Some(*btree_drain::set_drain_range(waiting, ..=dirty_state).last()?);
			Some(requester)
		}).collect()
	}
	// Returns the requesters that hold the data
	pub fn dirty_data(&mut self, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>) -> Vec<Entity> {
		let Some(requests) = self.requests.get_mut(params) else { return Vec::new(); };
		if let Some(dirties) = &mut requests.dirties && last_clean_tick >= requests.has_below {
			// A diff can not be applied after a newer one
			let diff = diff.filter(|_| requests.applied_through.is_none_or(|applied_through| last_clean_tick > applied_through));
			dirties.insert(dirties.partition_point(|&(tick, _)| tick <= last_clean_tick), (last_clean_tick, diff.cloned()));
		}
		requests.requesters.keys().copied().collect()
	}
	// If the stored data was passed as having the dirty already, it came late
	pub fn has_dirty(&self, params: &DT::Params, last_clean_tick: u64) -> bool {
		self.requests.get(params).is_some_and(|requests| last_clean_tick < requests.has_below)
	}
	// Returns what to cancel at which fulfiller. The params are no longer requested if every requester cancelled.
	pub fn cancel(&mut self, requester: Entity, params: &DT::Params, latest: DirtyState, oldest: Option<u64>) -> Vec<(Entity, DirtyState, Option<u64>)> {
		let Some(requests) = self.requests.get_mut(params) else { return Vec::new(); };
		let Some((waiting, usage)) = requests.requesters.get_mut(&requester) else { return Vec::new(); };
		let waited = !btree_drain::set_drain_range(waiting, cancel_range(latest, oldest)).is_empty();
		let used = usage.is_some_and(|usage| is_cancelled(usage, latest, oldest));
		if used { *usage = None; }
		if !waited && !used { return Vec::new(); }
		if waiting.is_empty() && usage.is_none() { requests.requesters.remove(&requester); }
		if requests.requesters.is_empty() {
			let Requests { fulfillers, diffing, .. } = self.requests.remove(params).expect("It was just found");
			return fulfillers.into_keys().chain(diffing.map(|(diff_fulfiller, ..)| diff_fulfiller)).map(|fulfiller| (fulfiller, DirtyState::UpToDate, None)).collect();
		}
		// Only what was passed on and nobody waits for anymore. A request served from the storage never reached the
		// fulfillers, so they keep their last state, or they would drop data that is still used.
		let Requests { requesters, fulfillers, diffing, dirties, source, .. } = requests;
		let unwaited = |dirty_state: &DirtyState| is_cancelled(*dirty_state, latest, oldest) && !requesters.values().any(|(waiting, _)| waiting.contains(dirty_state));
		let mut cancels = Vec::new();
		if fulfillers.values().any(|(passed_on, used)| used.is_some() || !passed_on.iter().all(unwaited)) {
			fulfillers.retain(|&fulfiller, (passed_on, used)| {
				let mut cancelled: Vec<DirtyState> = passed_on.iter().copied().filter(unwaited).collect();
				if *source == Some(fulfiller) && used.is_none() && cancelled.len() == passed_on.len() { cancelled.clear(); }
				let held = passed_on.iter().copied().filter(|dirty_state| !cancelled.contains(dirty_state)).chain(*used);
				let mut ranges = unheld_ranges(held, latest, oldest);
				ranges.retain(|&(latest, oldest)| cancelled.iter().any(|&dirty_state| is_cancelled(dirty_state, latest, oldest)));
				// What can not be cancelled without what it keeps stays in flight
				passed_on.retain(|&dirty_state| !ranges.iter().any(|&(latest, oldest)| is_cancelled(dirty_state, latest, oldest)));
				cancels.extend(ranges.into_iter().map(|(latest, oldest)| (fulfiller, latest, oldest)));
				!passed_on.is_empty() || used.is_some()
			});
		}
		if let Some((diff_fulfiller, Some(wanted), _)) = *diffing && unwaited(&wanted) {
			cancels.push((diff_fulfiller, latest, oldest));
			put_back(dirties.as_mut().expect("The diffs came from here"), diffing.take().expect("It was just found").2);
		}
		cancels
	}
}

// The state of data that lacks the dirties
fn state<D>(dirties: &[(u64, D)]) -> DirtyState {
	dirties.first().map_or(DirtyState::UpToDate, |&(tick, _)| DirtyState::Dirty { tick })
}

// Puts the dirties a diff applier was given back, before the ones of the same tick that came later
fn put_back<D>(dirties: &mut Vec<(u64, D)>, mut handed: Vec<(u64, D)>) {
	handed.append(dirties);
	handed.sort_by_key(|&(tick, _)| tick);
	*dirties = handed;
}

// ============================ Fulfillers ============================

pub enum Fulfilling<DT: DataType> {
	Applied(DT::Data), // The diffs were applied right away
	Applying(Entity), // The diff fulfiller passes the data later
	Requested(Entity), // The fulfiller passes the data later
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
	// bring it to a state the request allows. It returns the data with them applied right away, or passes it later.
	pub fn request(&mut self, request: Request<DT>, stale: Option<(DT::Data, Vec<DT::Diff>)>) -> Fulfilling<DT> {
		let fulfiller_params = &mut self.fulfiller_params;
		let fulfiller = self.fulfillers.iter()
			.filter_map(|(entity, fulfiller)| Some((fulfiller.cost(fulfiller_params, &request)?, entity)))
			.min_by(|a, b| a.0.total_cmp(&b.0));
		let diff_fulfiller = stale.as_ref().and_then(|(_, diffs)| self.diff_fulfillers.iter()
			.filter_map(|(entity, diff_fulfiller)| Some((diff_fulfiller.cost(fulfiller_params, &request, diffs)?, entity)))
			.min_by(|a, b| a.0.total_cmp(&b.0)));
		if let (Some((cost, entity)), Some((data, diffs))) = (diff_fulfiller, stale) && fulfiller.is_none_or(|(fulfiller_cost, _)| cost < fulfiller_cost) {
			let applied = self.diff_fulfillers.get_mut(entity).expect("It was just found").1.request(&mut self.fulfiller_params, request, data, diffs);
			return applied.map_or(Fulfilling::Applying(entity), Fulfilling::Applied);
		}
		// TODO: a failure path, for now a request nothing can fulfill is a bug
		let Some((_, entity)) = fulfiller else { panic!("No fulfiller of {} can fulfill the request", std::any::type_name::<DT>()); };
		self.fulfillers.get_mut(entity).expect("It was just found").1.request(&mut self.fulfiller_params, request);
		Fulfilling::Requested(entity)
	}
	pub fn cancel(&mut self, fulfiller: Entity, params: &DT::Params, latest: DirtyState, oldest: Option<u64>) {
		if let Ok((_, mut found)) = self.fulfillers.get_mut(fulfiller) {
			found.cancel(&mut self.fulfiller_params, params, latest, oldest);
		} else if let Ok((_, mut found)) = self.diff_fulfillers.get_mut(fulfiller) {
			found.cancel(&mut self.fulfiller_params, params, latest, oldest);
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
		let diffs = stale.as_ref().map_or(0, |(_, diffs)| diffs.len());
		let data = match storage_params.get::<StorageRequesterSystemParams<DT>>().request(request.clone(), stale) {
			Fulfilling::Applied(data) => data,
			Fulfilling::Applying(diff_fulfiller) => { self.requests.diffing(request, diff_fulfiller, diffs); return None; }
			Fulfilling::Requested(fulfiller) => { self.requests.passed_on(request, fulfiller); return None; }
		};
		let dirty_state = self.requests.applied(&request.params, diffs);
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
		for (fulfiller, latest, oldest) in cancel {
			fulfillers.cancel(fulfiller, params, latest, oldest);
		}
	}
}

impl<DT: DataType> StorageFulfillerAPI<DT> for Storage<DT> {
	fn pass_data(
		&mut self,
		storage_params: &mut DynStorageFulfillerParams<DT>,
		fulfiller: Entity,
		params: &DT::Params,
		dirty_state: DirtyState,
		data: DT::Data,
	) {
		let Some(dirty_state) = self.requests.answered(fulfiller, params, dirty_state) else { return; };
		if self.data.get(params).is_some_and(|(stored, _)| *stored > dirty_state) { return; }
		let requesters = self.requests.pass_data(fulfiller, params, dirty_state);
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
		if !self.requests.has_dirty(params, last_clean_tick) && let Some((dirty_state, _)) = self.data.get_mut(params) {
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
