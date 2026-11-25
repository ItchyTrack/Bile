use std::{collections::{HashMap, HashSet}, marker::PhantomData};

use bevy::prelude::*;
use bevy_trait_query::{One, RegisterExt};

use crate::{DataType, DirtyState, Request, cancel_range::{in_cancel_range, unheld_ranges}, dyn_system_params::{DynParam, RegisterDynParam}};
use super::api::{DynFulfillerParams, DynRequesterParams, DynStorageFulfillerParams, DynStorageRequesterParams, FulfillerAPI, RequesterAPI, StorageFulfillerAPI, StorageFulfillerParams, StorageRequesterAPI, StorageRequesterParams, StreamId};

// ============================ Streaming Storage ============================

pub struct StorageRequesterSystemParams<DT: DataType>(PhantomData<DT>);

impl<DT: DataType> DynParam<StorageRequesterParams<DT>> for StorageRequesterSystemParams<DT> {
	type Param = (Query<'static, 'static, (Entity, One<&'static mut dyn FulfillerAPI<DT>>)>, DynFulfillerParams<'static, 'static, DT>);
}

pub struct StorageFulfillerSystemParams<DT: DataType>(PhantomData<DT>);

impl<DT: DataType> DynParam<StorageFulfillerParams<DT>> for StorageFulfillerSystemParams<DT> {
	type Param = (Query<'static, 'static, One<&'static mut dyn RequesterAPI<DT>>>, DynRequesterParams<'static, 'static, DT>);
}

// Keeps no data. A request joins a stream of the same request until anything was passed on it, after that it gets a
// new stream that is requested from the fulfillers again.
#[derive(Resource)]
pub struct Storage<DT: DataType> {
	next_stream_id: u64,
	streams: HashMap<DT::Params, HashMap<StreamId, (DirtyState, HashSet<Entity>, bool)>>, // Params -> Stream -> The allowed tick it was requested with, its requesters, and if anything was passed on it
}

impl<DT: DataType> Default for Storage<DT> {
	fn default() -> Self { Self { next_stream_id: 0, streams: HashMap::default() } }
}

impl<DT: DataType> StorageRequesterAPI<DT> for Storage<DT> {
	fn request(
		&mut self,
		storage_params: &mut DynStorageRequesterParams<DT>,
		requester: Entity,
		request: Request<DT>,
	) -> StreamId {
		let streams = self.streams.entry(request.params.clone()).or_default();
		if let Some((&stream_id, (_, requesters, _))) = streams.iter_mut().find(|(_, (allowed_tick, _, passed))| *allowed_tick == request.allowed_tick && !passed) {
			requesters.insert(requester);
			return stream_id;
		}
		let stream_id = StreamId(self.next_stream_id);
		self.next_stream_id += 1;
		streams.insert(stream_id, (request.allowed_tick, HashSet::from([requester]), false));
		let (mut fulfillers, mut fulfiller_params) = storage_params.get::<StorageRequesterSystemParams<DT>>();
		let fulfiller = fulfillers.iter()
			.filter_map(|(entity, fulfiller)| Some((fulfiller.cost(&mut fulfiller_params, &request)?, entity)))
			.min_by(|a, b| a.0.total_cmp(&b.0));
		// TODO: a failure path, for now a request nothing can fulfill is a bug
		let Some((_, entity)) = fulfiller else { panic!("No fulfiller of {} can fulfill the request", std::any::type_name::<DT>()); };
		fulfillers.get_mut(entity).expect("It was just found").1.request(&mut fulfiller_params, stream_id, request);
		stream_id
	}

	fn cancel(
		&mut self,
		storage_params: &mut DynStorageRequesterParams<DT>,
		requester: Entity,
		params: &DT::Params,
		latest: DirtyState,
		oldest: Option<u64>, // None if everything before latest
	) {
		let Some(streams) = self.streams.get_mut(params) else { return; };
		let mut cancelled = Vec::new();
		streams.retain(|_, (allowed_tick, requesters, _)| {
			if !in_cancel_range(*allowed_tick, latest, oldest) || !requesters.remove(&requester) || !requesters.is_empty() { return true; }
			cancelled.push(*allowed_tick);
			false
		});
		let held: Vec<DirtyState> = streams.values().map(|(allowed_tick, _, _)| *allowed_tick).collect();
		if streams.is_empty() { self.streams.remove(params); }
		// Only what no other stream still holds, the fulfillers are not told which stream it was
		let (mut fulfillers, mut fulfiller_params) = storage_params.get::<StorageRequesterSystemParams<DT>>();
		for (latest, oldest) in unheld_ranges(held, latest, oldest) {
			if !cancelled.iter().any(|&allowed_tick| in_cancel_range(allowed_tick, latest, oldest)) { continue; }
			for (_, mut fulfiller) in &mut fulfillers {
				fulfiller.cancel(&mut fulfiller_params, params, latest, oldest);
			}
		}
	}
}

impl<DT: DataType> StorageFulfillerAPI<DT> for Storage<DT> {
	fn push(&mut self,
		storage_params: &mut DynStorageFulfillerParams<DT>,
		stream_id: StreamId,
		params: &DT::Params,
		dirty_state: DirtyState,
		data: DT::Data,
	) {
		let Some((_, requesters, passed)) = self.streams.get_mut(params).and_then(|streams| streams.get_mut(&stream_id)) else { return; };
		*passed = true;
		let (mut requester_apis, mut requester_params) = storage_params.get::<StorageFulfillerSystemParams<DT>>();
		for &requester in requesters.iter() {
			let Ok(mut requester) = requester_apis.get_mut(requester) else { continue; };
			requester.push(&mut requester_params, stream_id, params, dirty_state, data.clone());
		}
	}

	fn finish(&mut self,
		storage_params: &mut DynStorageFulfillerParams<DT>,
		stream_id: StreamId,
		params: &DT::Params,
	) {
		let Some((_, requesters, passed)) = self.streams.get_mut(params).and_then(|streams| streams.get_mut(&stream_id)) else { return; };
		*passed = true;
		let (mut requester_apis, mut requester_params) = storage_params.get::<StorageFulfillerSystemParams<DT>>();
		for &requester in requesters.iter() {
			let Ok(mut requester) = requester_apis.get_mut(requester) else { continue; };
			requester.finish(&mut requester_params, stream_id, params);
		}
	}

	fn dirty_data(&mut self,
		storage_params: &mut DynStorageFulfillerParams<DT>,
		params: &DT::Params,
		last_clean_tick: u64,
		diff: Option<&DT::Diff>,
	) {
		let Some(streams) = self.streams.get(params) else { return; };
		// Only the requesters that got something, once
		let requesters: HashSet<Entity> = streams.values().filter(|(_, _, passed)| *passed).flat_map(|(_, requesters, _)| requesters.iter().copied()).collect();
		let (mut requester_apis, mut requester_params) = storage_params.get::<StorageFulfillerSystemParams<DT>>();
		for requester in requesters {
			let Ok(mut requester) = requester_apis.get_mut(requester) else { continue; };
			requester.dirty_data(&mut requester_params, params, last_clean_tick, diff);
		}
	}
}

pub trait BileAddStreamingStorage {
	fn add_streaming_storage<DT: DataType>(&mut self) -> &mut Self;
}

impl BileAddStreamingStorage for App {
	fn add_streaming_storage<DT: DataType>(&mut self) -> &mut Self {
		self.init_resource::<Storage<DT>>()
			.register_component_as::<dyn StorageRequesterAPI<DT>, Storage<DT>>()
			.register_component_as::<dyn StorageFulfillerAPI<DT>, Storage<DT>>()
			.register_dyn_param::<StorageRequesterParams<DT>, StorageRequesterSystemParams<DT>>()
			.register_dyn_param::<StorageFulfillerParams<DT>, StorageFulfillerSystemParams<DT>>()
	}
}
