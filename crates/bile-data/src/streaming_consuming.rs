use std::{collections::{HashMap, HashSet}, marker::PhantomData};

use bevy::{ecs::{resource::IsResource, system::SystemParam}, prelude::*};
use bevy_trait_query::{One, RegisterExt};

use crate::{DataType, DirtyState, Request, cancel_range::{is_cancelled, unheld_ranges}, dyn_system_params::{DynParam, RegisterDynParam}};
use crate::streaming_api::{DynRequesterParams, DynStorageRequesterParams, RequesterAPI, RequesterParams, StorageRequesterAPI, StreamId};

// ============================ Consuming API ============================

#[bevy_trait_query::queryable]
pub trait ConsumingNodeAPI<DT: DataType> {
	fn push_data(&mut self, stream_id: StreamId, params: &DT::Params, dirty_state: DirtyState, data: DT::Data);
	fn finish_data(&mut self, stream_id: StreamId, params: &DT::Params);
	fn dirty_data(&mut self, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>);
}

#[derive(SystemParam)]
pub struct Requester<'w, 's, DT: DataType> {
	requester: ResMut<'w, RequesterBookkeeping<DT>>,
	storage: Single<'w, 's, One<&'static mut dyn StorageRequesterAPI<DT>>, With<IsResource>>,
	storage_params: DynStorageRequesterParams<'w, 's, DT>,
}

pub struct RequesterSystemParams<DT: DataType>(PhantomData<DT>);

impl<DT: DataType> DynParam<RequesterParams<DT>> for RequesterSystemParams<DT> {
	type Param = Query<'static, 'static, One<&'static mut dyn ConsumingNodeAPI<DT>>>;
}

#[derive(Resource)]
pub struct RequesterBookkeeping<DT: DataType> {
	entity: Entity, // The resource entity, the storage knows the requester by it
	streams: HashMap<DT::Params, HashMap<StreamId, (DirtyState, HashSet<Entity>, bool)>>, // Params -> Stream -> The allowed tick it was requested with, the consuming nodes on it, and if anything was passed on it
}

impl<DT: DataType> RequesterAPI<DT> for RequesterBookkeeping<DT> {
	fn push(&mut self,
		requester_params: &mut DynRequesterParams<DT>,
		stream_id: StreamId,
		params: &DT::Params,
		dirty_state: DirtyState,
		data: DT::Data,
	) {
		let Some((_, nodes, passed)) = self.streams.get_mut(params).and_then(|streams| streams.get_mut(&stream_id)) else { return; };
		*passed = true;
		let mut consuming_nodes = requester_params.get::<RequesterSystemParams<DT>>();
		for &entity in nodes.iter() {
			let Ok(mut node) = consuming_nodes.get_mut(entity) else { continue; };
			node.push_data(stream_id, params, dirty_state, data.clone());
		}
	}
	fn finish(&mut self,
		requester_params: &mut DynRequesterParams<DT>,
		stream_id: StreamId,
		params: &DT::Params,
	) {
		let Some((_, nodes, passed)) = self.streams.get_mut(params).and_then(|streams| streams.get_mut(&stream_id)) else { return; };
		*passed = true;
		let mut consuming_nodes = requester_params.get::<RequesterSystemParams<DT>>();
		for &entity in nodes.iter() {
			let Ok(mut node) = consuming_nodes.get_mut(entity) else { continue; };
			node.finish_data(stream_id, params);
		}
	}
	fn dirty_data(&mut self,
		requester_params: &mut DynRequesterParams<DT>,
		params: &DT::Params,
		last_clean_tick: u64,
		diff: Option<&DT::Diff>,
	) {
		let Some(streams) = self.streams.get(params) else { return; };
		// Only the nodes that got something, once
		let nodes: HashSet<Entity> = streams.values().filter(|(_, _, passed)| *passed).flat_map(|(_, nodes, _)| nodes.iter().copied()).collect();
		let mut consuming_nodes = requester_params.get::<RequesterSystemParams<DT>>();
		for entity in nodes {
			let Ok(mut node) = consuming_nodes.get_mut(entity) else { continue; };
			node.dirty_data(params, last_clean_tick, diff);
		}
	}
}

impl<DT: DataType> Requester<'_, '_, DT> {
	pub fn request(&mut self, request: Request<DT>, requester_node: Entity) -> StreamId {
		let (params, allowed_tick) = (request.params.clone(), request.allowed_tick);
		let stream_id = self.storage.request(&mut self.storage_params, self.requester.entity, request);
		let (_, nodes, _) = self.requester.streams.entry(params).or_default().entry(stream_id).or_insert_with(|| (allowed_tick, HashSet::new(), false));
		nodes.insert(requester_node);
		stream_id
	}
	pub fn cancel(&mut self, params: &DT::Params, latest: DirtyState, oldest: Option<u64>, requester_node: Entity) {
		let requester = &mut *self.requester;
		let Some(streams) = requester.streams.get_mut(params) else { return; };
		let mut cancelled = Vec::new();
		streams.retain(|_, (allowed_tick, nodes, _)| {
			if !is_cancelled(*allowed_tick, latest, oldest) || !nodes.remove(&requester_node) || !nodes.is_empty() { return true; }
			cancelled.push(*allowed_tick);
			false
		});
		let held: Vec<DirtyState> = streams.values().map(|(allowed_tick, _, _)| *allowed_tick).collect();
		if streams.is_empty() { requester.streams.remove(params); }
		// Only what no other node still holds
		for (latest, oldest) in unheld_ranges(held, latest, oldest) {
			if cancelled.iter().any(|&allowed_tick| is_cancelled(allowed_tick, latest, oldest)) { self.storage.cancel(&mut self.storage_params, requester.entity, params, latest, oldest); }
		}
	}
}

pub trait BileRegisterStreamingConsumer {
	fn register_consumer_streaming<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + ConsumingNodeAPI<DT>;
}

impl BileRegisterStreamingConsumer for App {
	fn register_consumer_streaming<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + ConsumingNodeAPI<DT>,
	{
		if !self.world().contains_resource::<RequesterBookkeeping<DT>>() {
			self.insert_resource(RequesterBookkeeping::<DT> { entity: Entity::PLACEHOLDER, streams: HashMap::default() });
			let entity = self.world().resource_entity::<RequesterBookkeeping<DT>>().expect("It was just inserted");
			self.world_mut().resource_mut::<RequesterBookkeeping<DT>>().entity = entity;
		}
		self.register_component_as::<dyn ConsumingNodeAPI<DT>, C>()
			.register_component_as::<dyn RequesterAPI<DT>, RequesterBookkeeping<DT>>()
			.register_dyn_param::<RequesterParams<DT>, RequesterSystemParams<DT>>()
	}
}
