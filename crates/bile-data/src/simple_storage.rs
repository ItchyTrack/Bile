use std::collections::HashMap;
use std::marker::PhantomData;

use bevy::{ecs::system::SystemParam, prelude::*};
use bevy_trait_query::{One, RegisterExt};

use crate::diff::DiffFulfillerAPI;
use crate::dyn_system_params::{DynParam, RegisterDynParam};
use crate::{unheld_ranges, BileDataSchedule, BileDataSystemSet, Claim, DataType, DirtyState, Request};
use crate::simple_api::{DynFulfillerParams, DynRequesterParams, DynStorageFulfillerParams, DynStorageRequesterParams, FulfillerAPI, RequesterAPI, StorageFulfillerAPI, StorageFulfillerParams, StorageRequesterAPI, StorageRequesterParams};

// ============================ Storage Requests ============================

// The bookkeeping of the requests a storage got, for any storage to use
pub struct StorageRequests<DT: DataType> {
	requests: HashMap<DT::Params, Requests<DT>>,
	applied: Vec<DT::Params>, // The diffs were applied right away, the requesters that wait get the data in the PassData stage
}

struct Requests<DT: DataType> {
	requesters: HashMap<Entity, Claim>,
	fulfillers: Claim, // What was passed on to the fulfillers
	diffs: Option<Vec<DT::Diff>>, // Every diff since the data was up to date. None if a dirty had no diff
}

impl<DT: DataType> Default for StorageRequests<DT> {
	fn default() -> Self { Self { requests: HashMap::default(), applied: Vec::new() } }
}

impl<DT: DataType> Default for Requests<DT> {
	fn default() -> Self { Self { requesters: HashMap::default(), fulfillers: Claim::default(), diffs: None } }
}

impl<DT: DataType> StorageRequests<DT> {
	pub fn is_requested(&self, params: &DT::Params) -> bool { self.requests.contains_key(params) }
	pub fn request(&mut self, requester: Entity, request: &Request<DT>, served: bool) {
		self.requests.entry(request.params.clone()).or_default().requesters.entry(requester).or_default().request(request.allowed_tick, served);
	}
	pub fn is_in_flight(&self, request: &Request<DT>) -> bool {
		self.requests.get(&request.params).is_some_and(|requests| requests.fulfillers.waiting.contains(&request.allowed_tick))
	}
	pub fn passed_on(&mut self, request: &Request<DT>) {
		let Some(requests) = self.requests.get_mut(&request.params) else { return; };
		requests.fulfillers.request(request.allowed_tick, false);
	}
	// The diffs to bring the stored data up to date
	pub fn diffs(&self, params: &DT::Params) -> Option<&[DT::Diff]> { self.requests.get(params)?.diffs.as_deref() }
	pub fn applied(&mut self, params: &DT::Params) {
		let Some(requests) = self.requests.get_mut(params) else { return; };
		requests.diffs = Some(Vec::new());
		self.applied.push(params.clone());
	}
	pub fn take_applied(&mut self) -> Vec<DT::Params> { std::mem::take(&mut self.applied) }
	// A fulfiller passed data, even if it is older than the stored data. Returns false if the params are not requested.
	pub fn answered(&mut self, params: &DT::Params, dirty_state: DirtyState) -> bool {
		let Some(requests) = self.requests.get_mut(params) else { return false; };
		requests.fulfillers.pass(dirty_state);
		true
	}
	// New data was stored. Returns the requesters that waited for something it satisfies
	pub fn pass_data(&mut self, params: &DT::Params, dirty_state: DirtyState) -> Vec<Entity> {
		let Some(requests) = self.requests.get_mut(params) else { return Vec::new(); };
		requests.diffs = (dirty_state == DirtyState::UpToDate).then(Vec::new);
		self.satisfied(params, dirty_state)
	}
	// Returns the requesters that waited for something the stored data satisfies
	pub fn satisfied(&mut self, params: &DT::Params, dirty_state: DirtyState) -> Vec<Entity> {
		let Some(requests) = self.requests.get_mut(params) else { return Vec::new(); };
		requests.requesters.iter_mut().filter_map(|(&requester, claim)| claim.pass(dirty_state).then_some(requester)).collect()
	}
	// Returns the requesters that hold the data
	pub fn dirty_data(&mut self, params: &DT::Params, diff: Option<&DT::Diff>) -> Vec<Entity> {
		let Some(requests) = self.requests.get_mut(params) else { return Vec::new(); };
		match (&mut requests.diffs, diff) {
			(Some(diffs), Some(diff)) => diffs.push(diff.clone()),
			_ => requests.diffs = None, // A dirty without a diff ends the run
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
			let held = !requests.fulfillers.is_empty();
			self.requests.remove(params);
			return if held { vec![(DirtyState::UpToDate, None)] } else { Vec::new() };
		}
		// Only what was passed on and nobody waits for anymore. A request served from the storage never reached the
		// fulfillers, so they keep their last state, or they would drop data that is still used.
		let fulfillers = &mut requests.fulfillers;
		let unwaited: Vec<DirtyState> = fulfillers.waiting.iter().copied()
			.filter(|dirty_state| dirty_state.is_cancelled_by(latest, oldest) && !requests.requesters.values().any(|claim| claim.waiting.contains(dirty_state)))
			.collect();
		if unwaited.is_empty() || fulfillers.usage.is_none() && unwaited.len() == fulfillers.waiting.len() { return Vec::new(); }
		let held = fulfillers.waiting.iter().copied().filter(|dirty_state| !unwaited.contains(dirty_state)).chain(fulfillers.usage);
		let mut ranges = unheld_ranges(held, latest, oldest);
		ranges.retain(|&(latest, oldest)| unwaited.iter().any(|dirty_state| dirty_state.is_cancelled_by(latest, oldest)));
		// What can not be cancelled without the usage stays in flight
		fulfillers.waiting.retain(|dirty_state| !ranges.iter().any(|&(latest, oldest)| dirty_state.is_cancelled_by(latest, oldest)));
		ranges
	}
}

// ============================ Fulfillers ============================

// Reaches the fulfillers of a data type, for any storage to use
#[derive(SystemParam)]
pub struct Fulfillers<'w, 's, DT: DataType> {
	fulfillers: Query<'w, 's, (Entity, One<&'static mut dyn FulfillerAPI<DT>>)>,
	diff_fulfillers: Query<'w, 's, (Entity, One<&'static mut dyn DiffFulfillerAPI<DT>>)>,
	fulfiller_params: DynFulfillerParams<'w, 's, DT>,
}

impl<DT: DataType> Fulfillers<'_, '_, DT> {
	// Hands the request to the cheapest fulfiller. That may be a diff fulfiller if there is stale data and the diffs
	// to bring it up to date, which returns the up to date data right away.
	pub fn request(&mut self, request: Request<DT>, stale: Option<(DT::Data, &[DT::Diff])>) -> Option<DT::Data> {
		let fulfiller_params = &mut self.fulfiller_params;
		let fulfiller = self.fulfillers.iter()
			.filter_map(|(entity, fulfiller)| Some((fulfiller.cost(fulfiller_params, &request)?, entity)))
			.min_by(|a, b| a.0.total_cmp(&b.0));
		let diff_fulfiller = stale.as_ref().and_then(|(_, diffs)| self.diff_fulfillers.iter()
			.filter_map(|(entity, diff_fulfiller)| Some((diff_fulfiller.cost(fulfiller_params, &request, diffs)?, entity)))
			.min_by(|a, b| a.0.total_cmp(&b.0)));
		if let (Some((cost, entity)), Some((data, diffs))) = (diff_fulfiller, stale) && fulfiller.is_none_or(|(fulfiller_cost, _)| cost < fulfiller_cost)
			&& let Some(data) = self.diff_fulfillers.get_mut(entity).expect("It was just found").1.request(&mut self.fulfiller_params, request.clone(), data, diffs.to_vec()) {
			return Some(data);
		}
		// TODO: a failure path, for now a request nothing can fulfill is a bug
		let Some((_, entity)) = fulfiller else { panic!("No fulfiller of {} can fulfill the request", std::any::type_name::<DT>()); };
		self.fulfillers.get_mut(entity).expect("It was just found").1.request(&mut self.fulfiller_params, request);
		None
	}
	pub fn cancel(&mut self, params: &DT::Params, latest: DirtyState, oldest: Option<u64>) {
		for (_, mut fulfiller) in &mut self.fulfillers {
			fulfiller.cancel(&mut self.fulfiller_params, params, latest, oldest);
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
		let stale = stored.and_then(|(_, data)| Some((data.clone(), self.requests.diffs(&request.params)?)));
		let mut fulfillers = storage_params.get::<StorageRequesterSystemParams<DT>>();
		let applied = fulfillers.request(request.clone(), stale);
		self.requests.request(requester, request, applied.is_some());
		let Some(data) = applied else {
			self.requests.passed_on(request);
			return None;
		};
		// The diffs were applied right away
		self.data.insert(request.params.clone(), (DirtyState::UpToDate, Some(data.clone())));
		self.requests.applied(&request.params);
		Some((data, DirtyState::UpToDate))
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
		if !self.requests.answered(params, dirty_state) { return; }
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
		let requesters = self.requests.dirty_data(params, diff);
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
			.add_systems(BileDataSchedule, pass_applied::<DT>.in_set(BileDataSystemSet::PassData))
	}
}
