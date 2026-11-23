use std::collections::{BTreeSet, HashMap};
use std::marker::PhantomData;

use bevy::{ecs::system::SystemParam, prelude::*};
use bevy_trait_query::{One, RegisterExt};

use crate::diff::DiffFulfillerAPI;
use crate::dyn_system_params::{DynParam, RegisterDynParam};
use crate::{btree_drain, cancel_range, unheld_ranges, DataType, DirtyState, Request};
use crate::simple_api::{DynFulfillerParams, DynRequesterParams, DynStorageFulfillerParams, DynStorageRequesterParams, FulfillerAPI, RequesterAPI, StorageFulfillerAPI, StorageFulfillerParams, StorageRequesterAPI, StorageRequesterParams};

// ============================ Storage Requests ============================

// The bookkeeping of the requests a storage got, for any storage to use
pub struct StorageRequests<DT: DataType> {
	requests: HashMap<DT::Params, Requests<DT>>,
}

struct Requests<DT: DataType> {
	requested: HashMap<Entity, BTreeSet<DirtyState>>, // Requester -> Requested dirty states that were not cancelled
	in_flight: BTreeSet<DirtyState>, // Passed on to the fulfillers, no data that satisfies them was passed yet
	held: BTreeSet<DirtyState>, // Passed on to the fulfillers and not cancelled there
	diffs: Option<Vec<DT::Diff>>, // Every diff since the data was up to date. None if a dirty had no diff
}

impl<DT: DataType> Default for StorageRequests<DT> {
	fn default() -> Self { Self { requests: HashMap::default() } }
}

impl<DT: DataType> Default for Requests<DT> {
	fn default() -> Self { Self { requested: HashMap::default(), in_flight: BTreeSet::new(), held: BTreeSet::new(), diffs: None } }
}

impl<DT: DataType> StorageRequests<DT> {
	pub fn is_requested(&self, params: &DT::Params) -> bool { self.requests.contains_key(params) }
	pub fn request(&mut self, requester: Entity, request: &Request<DT>) {
		self.requests.entry(request.params.clone()).or_default().requested.entry(requester).or_default().insert(request.allowed_tick);
	}
	pub fn is_in_flight(&self, request: &Request<DT>) -> bool {
		self.requests.get(&request.params).is_some_and(|requests| requests.in_flight.contains(&request.allowed_tick))
	}
	pub fn passed_on(&mut self, request: &Request<DT>) {
		let Some(requests) = self.requests.get_mut(&request.params) else { return; };
		requests.in_flight.insert(request.allowed_tick);
		requests.held.insert(request.allowed_tick);
	}
	// The diffs to bring the stored data up to date
	pub fn diffs(&self, params: &DT::Params) -> Option<&[DT::Diff]> { self.requests.get(params)?.diffs.as_deref() }
	// Returns the requesters that requested something the data satisfies
	pub fn pass_data(&mut self, params: &DT::Params, dirty_state: DirtyState) -> Vec<Entity> {
		let Some(requests) = self.requests.get_mut(params) else { return Vec::new(); };
		requests.in_flight.retain(|&in_flight| in_flight > dirty_state);
		requests.diffs = (dirty_state == DirtyState::UpToDate).then(Vec::new);
		requests.requested.iter().filter(|(_, requested)| requested.first().is_some_and(|&oldest| oldest <= dirty_state)).map(|(&requester, _)| requester).collect()
	}
	// Returns the requesters that requested the data
	pub fn dirty_data(&mut self, params: &DT::Params, diff: Option<&DT::Diff>) -> Vec<Entity> {
		let Some(requests) = self.requests.get_mut(params) else { return Vec::new(); };
		match (&mut requests.diffs, diff) {
			(Some(diffs), Some(diff)) => diffs.push(diff.clone()),
			_ => requests.diffs = None, // A dirty without a diff ends the run
		}
		requests.requested.keys().copied().collect()
	}
	// Returns what to cancel at the fulfillers. The params are no longer requested if every requester cancelled.
	pub fn cancel(&mut self, requester: Entity, params: &DT::Params, latest: DirtyState, oldest: Option<u64>) -> Vec<(DirtyState, Option<u64>)> {
		let Some(requests) = self.requests.get_mut(params) else { return Vec::new(); };
		let Some(requested) = requests.requested.get_mut(&requester) else { return Vec::new(); };
		if btree_drain::set_drain_range(requested, cancel_range(latest, oldest)).is_empty() { return Vec::new(); }
		if requested.is_empty() { requests.requested.remove(&requester); }
		if requests.requested.is_empty() {
			let held = !requests.held.is_empty();
			self.requests.remove(params);
			return if held { vec![(DirtyState::UpToDate, None)] } else { Vec::new() };
		}
		let mut ranges = unheld_ranges(requests.requested.values().flatten().copied(), latest, oldest);
		// A request served from the storage never reached the fulfillers. So the fulfillers have to keep some state,
		// or they would drop data that is still used. Only what is in flight is worth cancelling.
		ranges.retain(|&(latest, oldest)| {
			let cancelled = btree_drain::set_drain_range(&mut requests.held, cancel_range(latest, oldest));
			if requests.held.is_empty() || !cancelled.iter().any(|dirty_state| requests.in_flight.contains(dirty_state)) {
				requests.held.extend(cancelled);
				return false;
			}
			requests.in_flight.retain(|dirty_state| !cancelled.contains(dirty_state));
			true
		});
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
	// to bring it up to date, which may return the up to date data right away.
	pub fn request(&mut self, request: Request<DT>, stale: Option<(DT::Data, &[DT::Diff])>) -> Option<DT::Data> {
		let fulfiller_params = &mut self.fulfiller_params;
		let fulfiller = self.fulfillers.iter()
			.filter_map(|(entity, fulfiller)| Some((fulfiller.cost(fulfiller_params, &request)?, entity)))
			.min_by(|a, b| a.0.total_cmp(&b.0));
		let diff_fulfiller = stale.as_ref().and_then(|(_, diffs)| self.diff_fulfillers.iter()
			.filter_map(|(entity, diff_fulfiller)| Some((diff_fulfiller.cost(fulfiller_params, &request, diffs)?, entity)))
			.min_by(|a, b| a.0.total_cmp(&b.0)));
		if let (Some((cost, entity)), Some((data, diffs))) = (diff_fulfiller, stale) && fulfiller.is_none_or(|(fulfiller_cost, _)| cost < fulfiller_cost) {
			let (_, mut diff_fulfiller) = self.diff_fulfillers.get_mut(entity).expect("It was just found");
			return diff_fulfiller.request(&mut self.fulfiller_params, request, data, diffs.to_vec());
		}
		let Some((_, entity)) = fulfiller else {
			bevy::log::warn!("No fulfiller of {} can fulfill the request.", std::any::type_name::<DT>());
			return None;
		};
		self.fulfillers.get_mut(entity).expect("It was just found").1.request(&mut self.fulfiller_params, request);
		None
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
}

impl<DT: DataType> StorageRequesterAPI<DT> for Storage<DT> {
	fn request(
		&mut self,
		storage_params: &mut DynStorageRequesterParams<DT>,
		requester: Entity,
		request: &Request<DT>,
	) -> Option<(DT::Data, DirtyState)>  {
		self.requests.request(requester, request);
		let stored = self.data.get(&request.params).and_then(|(dirty_state, data)| Some((*dirty_state, data.as_ref()?)));
		if let Some((dirty_state, data)) = stored && dirty_state >= request.allowed_tick {
			return Some((data.clone(), dirty_state));
		}
		if self.requests.is_in_flight(request) { return None; }
		let stale = stored.and_then(|(_, data)| Some((data.clone(), self.requests.diffs(&request.params)?)));
		let mut fulfillers = storage_params.get::<StorageRequesterSystemParams<DT>>();
		let Some(data) = fulfillers.request(request.clone(), stale) else {
			self.requests.passed_on(request);
			return None;
		};
		// The diffs were applied right away
		self.data.insert(request.params.clone(), (DirtyState::UpToDate, Some(data.clone())));
		self.requests.pass_data(&request.params, DirtyState::UpToDate);
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
		if !self.requests.is_requested(params) { return; }
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
	}
}
