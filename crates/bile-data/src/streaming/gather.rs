use std::{collections::{HashMap, HashSet}, marker::PhantomData};

use bevy::{ecs::{resource::IsResource, system::SystemParam}, prelude::*};
use bevy_trait_query::{One, RegisterExt};

use crate::{DataType, DirtyState, Request, StateTick, cancel_range::in_cancel_range, dyn_system_params::{DynParam, RegisterDynParam}};
use super::api::{DynFulfillerParams, DynStorageFulfillerParams, FulfillerAPI, FulfillerParams, StorageFulfillerAPI, StreamId};

// ============================ Producing API ============================

// A node produces a part of the data
#[bevy_trait_query::queryable]
pub trait ProducingNodeAPI<DT: DataType> {
	fn cost(&self, request: &Request<DT>) -> Option<f32>; // None if the node has no part of the data
	fn request(&mut self, stream_id: StreamId, request: Request<DT>);
	fn cancel(&mut self, params: &DT::Params, latest: DirtyState, oldest: Option<u64>);
}

// ============================ Gather Fulfiller ============================

pub struct GatherSystemParams<DT: DataType>(PhantomData<DT>);

impl<DT: DataType> DynParam<FulfillerParams<DT>> for GatherSystemParams<DT> {
	type Param = Query<'static, 'static, (Entity, One<&'static mut dyn ProducingNodeAPI<DT>>)>;
}

// Every node that can is asked and streams its part into the same stream, which is finished when all of them finished
#[derive(Resource)]
pub struct GatherFulfiller<DT: DataType> {
	streams: HashMap<DT::Params, HashMap<StreamId, (DirtyState, HashMap<Entity, bool>)>>, // Params -> Stream -> The allowed tick it was requested with, and its nodes with if they still stream
}

impl<DT: DataType> Default for GatherFulfiller<DT> {
	fn default() -> Self { Self { streams: HashMap::default() } }
}

impl<DT: DataType> GatherFulfiller<DT> {
	fn is_streaming(&self, source_entity: Entity, stream_id: StreamId, params: &DT::Params) -> bool {
		self.streams.get(params).and_then(|streams| streams.get(&stream_id)?.1.get(&source_entity)).is_some_and(|&streaming| streaming)
	}
	// Returns if the node finished the last part of the stream, or None if it does not stream a part of it
	fn finish(&mut self, source_entity: Entity, stream_id: StreamId, params: &DT::Params) -> Option<bool> {
		let (_, nodes) = self.streams.get_mut(params)?.get_mut(&stream_id)?;
		let streaming = nodes.get_mut(&source_entity).filter(|streaming| **streaming)?;
		*streaming = false;
		Some(!nodes.values().any(|&streaming| streaming))
	}
}

impl<DT: DataType> FulfillerAPI<DT> for GatherFulfiller<DT> {
	fn cost(
		&self,
		fulfiller_params: &mut DynFulfillerParams<DT>,
		request: &Request<DT>,
	) -> Option<f32> {
		// The nodes stream at the same time, so it costs as much as the most expensive of them
		fulfiller_params.get::<GatherSystemParams<DT>>().iter().filter_map(|(_, node)| node.cost(request)).reduce(f32::max)
	}
	fn request(
		&mut self,
		fulfiller_params: &mut DynFulfillerParams<DT>,
		stream_id: StreamId,
		request: Request<DT>,
	) {
		let mut nodes = HashMap::new();
		for (entity, mut node) in &mut fulfiller_params.get::<GatherSystemParams<DT>>() {
			if node.cost(&request).is_none() { continue; }
			node.request(stream_id, request.clone());
			nodes.insert(entity, true);
		}
		self.streams.entry(request.params).or_default().insert(stream_id, (request.allowed_tick, nodes));
	}
	fn cancel(
		&mut self,
		fulfiller_params: &mut DynFulfillerParams<DT>,
		params: &DT::Params,
		latest: DirtyState,
		oldest: Option<u64>, // None if everything before latest
	) {
		let Some(streams) = self.streams.get_mut(params) else { return; };
		let mut nodes = HashSet::new();
		streams.retain(|_, (allowed_tick, stream_nodes)| {
			if !in_cancel_range(*allowed_tick, latest, oldest) { return true; }
			nodes.extend(stream_nodes.keys().copied());
			false
		});
		if streams.is_empty() { self.streams.remove(params); }
		let mut producing_nodes = fulfiller_params.get::<GatherSystemParams<DT>>();
		for entity in nodes {
			if let Ok((_, mut node)) = producing_nodes.get_mut(entity) { node.cancel(params, latest, oldest); }
		}
	}
}

// ============================ Data Passer ============================

#[derive(SystemParam)]
pub struct DataPasser<'w, 's, DT: DataType> {
	gather: ResMut<'w, GatherFulfiller<DT>>,
	storage: Single<'w, 's, One<&'static mut dyn StorageFulfillerAPI<DT>>, With<IsResource>>,
	storage_params: DynStorageFulfillerParams<'w, 's, DT>,
}

impl<DT: DataType> DataPasser<'_, '_, DT> {
	// Only from a node that was asked for a part of the stream, until it finished its part
	pub fn push(&mut self, source_entity: Entity, stream_id: StreamId, params: &DT::Params, dirty_state: DirtyState, data: DT::Data) {
		if !self.gather.is_streaming(source_entity, stream_id, params) { return; }
		self.storage.push(&mut self.storage_params, stream_id, dirty_state, data);
	}
	// The stream is finished when every node finished its part
	pub fn finish(&mut self, source_entity: Entity, stream_id: StreamId, params: &DT::Params) {
		if self.gather.finish(source_entity, stream_id, params) != Some(true) { return; }
		self.storage.finish(&mut self.storage_params, stream_id);
	}
}

// ============================ Data Dirtier ============================

#[derive(SystemParam)]
pub struct DataDirtier<'w, 's, DT: DataType> {
	bile_state_tick: Res<'w, StateTick>,
	storage: Single<'w, 's, One<&'static mut dyn StorageFulfillerAPI<DT>>, With<IsResource>>,
	storage_params: DynStorageFulfillerParams<'w, 's, DT>,
}

impl<DT: DataType> DataDirtier<'_, '_, DT> {
	// The tick a leaf source dirties with, a node that passes on a dirty uses the tick it got
	pub fn current_tick(&self) -> u64 {
		self.bile_state_tick.current_tick()
	}
	pub fn mark_dirty(&mut self, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>) {
		self.storage.dirty_data(&mut self.storage_params, params, last_clean_tick, diff);
	}
}

pub trait BileRegisterGather {
	fn register_producer_gather<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + ProducingNodeAPI<DT>;
}

impl BileRegisterGather for App {
	fn register_producer_gather<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + ProducingNodeAPI<DT>,
	{
		self.init_resource::<GatherFulfiller<DT>>()
			.register_component_as::<dyn ProducingNodeAPI<DT>, C>()
			.register_component_as::<dyn FulfillerAPI<DT>, GatherFulfiller<DT>>()
			.register_dyn_param::<FulfillerParams<DT>, GatherSystemParams<DT>>()
	}
}
