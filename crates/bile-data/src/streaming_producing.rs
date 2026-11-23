use std::{collections::{HashMap, HashSet}, marker::PhantomData};

use bevy::{ecs::{resource::IsResource, system::SystemParam}, prelude::*};
use bevy_trait_query::{One, RegisterExt};

use crate::{DataType, DirtyState, Request, StateTick, cancel_range::is_cancelled, dyn_system_params::{DynParam, RegisterDynParam}, gather::GatherFulfiller};
use crate::streaming_api::{DynFulfillerParams, DynStorageFulfillerParams, FulfillerAPI, FulfillerParams, StorageFulfillerAPI, StreamId};

// ============================ Producing API ============================

#[bevy_trait_query::queryable]
pub trait ProducingNodeAPI<DT: DataType> {
	fn cost(
		&self,
		request: &Request<DT>,
	) -> Option<f32>; // None if the node can not fulfill the request
	fn request(
		&mut self,
		stream_id: StreamId,
		request: Request<DT>,
	);
	fn cancel(
		&mut self,
		params: &DT::Params,
		latest: DirtyState,
		oldest: Option<u64>, // None if everything before latest
	);
}

// ============================ Fulfiller ============================

pub struct FulfillerSystemParams<DT: DataType>(PhantomData<DT>);

impl<DT: DataType> DynParam<FulfillerParams<DT>> for FulfillerSystemParams<DT> {
	type Param = Query<'static, 'static, (Entity, One<&'static mut dyn ProducingNodeAPI<DT>>)>;
}

// None if no node can fulfill the request
fn best_node<DT: DataType>(producing_nodes: &Query<(Entity, One<&'static mut dyn ProducingNodeAPI<DT>>)>, request: &Request<DT>) -> Option<(f32, Entity)> {
	producing_nodes.iter().filter_map(|(entity, node)| Some((node.cost(request)?, entity))).min_by(|a, b| a.0.total_cmp(&b.0))
}

// The cheapest node streams the data
#[derive(Resource)]
pub struct FulfillerBookkeeping<DT: DataType> {
	asked: HashMap<DT::Params, HashMap<StreamId, (DirtyState, Entity)>>, // Params -> Stream -> The allowed tick it was requested with, and the node that streams it
}

impl<DT: DataType> Default for FulfillerBookkeeping<DT> {
	fn default() -> Self { Self { asked: HashMap::default() } }
}

impl<DT: DataType> FulfillerAPI<DT> for FulfillerBookkeeping<DT> {
	fn cost(
		&self,
		fulfiller_params: &mut DynFulfillerParams<DT>,
		request: &Request<DT>,
	) -> Option<f32> {
		best_node(&fulfiller_params.get::<FulfillerSystemParams<DT>>(), request).map(|(cost, _)| cost)
	}
	fn request(
		&mut self,
		fulfiller_params: &mut DynFulfillerParams<DT>,
		stream_id: StreamId,
		request: Request<DT>,
	) {
		let mut producing_nodes = fulfiller_params.get::<FulfillerSystemParams<DT>>();
		let Some((_, entity)) = best_node(&producing_nodes, &request) else { return; };
		self.asked.entry(request.params.clone()).or_default().insert(stream_id, (request.allowed_tick, entity));
		producing_nodes.get_mut(entity).expect("It was just found").1.request(stream_id, request);
	}
	fn cancel(
		&mut self,
		fulfiller_params: &mut DynFulfillerParams<DT>,
		params: &DT::Params,
		latest: DirtyState,
		oldest: Option<u64>, // None if everything before latest
	) {
		let Some(streams) = self.asked.get_mut(params) else { return; };
		let mut nodes = HashSet::new();
		streams.retain(|_, &mut (allowed_tick, entity)| {
			if !is_cancelled(allowed_tick, latest, oldest) { return true; }
			nodes.insert(entity);
			false
		});
		if streams.is_empty() { self.asked.remove(params); }
		let mut producing_nodes = fulfiller_params.get::<FulfillerSystemParams<DT>>();
		for entity in nodes {
			if let Ok((_, mut node)) = producing_nodes.get_mut(entity) { node.cancel(params, latest, oldest); }
		}
	}
}

// ============================ Data Passer ============================

#[derive(SystemParam)]
pub struct DataPasser<'w, 's, DT: DataType> {
	fulfiller: Option<Res<'w, FulfillerBookkeeping<DT>>>,
	gather: Option<ResMut<'w, GatherFulfiller<DT>>>,
	storage: Single<'w, 's, One<&'static mut dyn StorageFulfillerAPI<DT>>, With<IsResource>>,
	storage_params: DynStorageFulfillerParams<'w, 's, DT>,
}

impl<DT: DataType> DataPasser<'_, '_, DT> {
	// Only from the nodes that were asked for the stream
	fn is_asked(&self, source_entity: Entity, stream_id: StreamId, params: &DT::Params) -> bool {
		self.fulfiller.as_ref().and_then(|fulfiller| fulfiller.asked.get(params)?.get(&stream_id)).is_some_and(|&(_, entity)| entity == source_entity)
	}
	pub fn push(&mut self, source_entity: Entity, stream_id: StreamId, params: &DT::Params, dirty_state: DirtyState, data: DT::Data) {
		let gathered = self.gather.as_ref().is_some_and(|gather| gather.is_streaming(source_entity, stream_id, params));
		if !gathered && !self.is_asked(source_entity, stream_id, params) { return; }
		self.storage.push(&mut self.storage_params, stream_id, params, dirty_state, data);
	}
	pub fn finish(&mut self, source_entity: Entity, stream_id: StreamId, params: &DT::Params) {
		// A gathered stream is finished when every node of it finished
		let gathered = self.gather.as_mut().and_then(|gather| gather.finish(source_entity, stream_id, params));
		if gathered == Some(false) || gathered.is_none() && !self.is_asked(source_entity, stream_id, params) { return; }
		self.storage.finish(&mut self.storage_params, stream_id, params);
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

pub trait BileRegisterStreamingProducer {
	fn register_producer_streaming<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + ProducingNodeAPI<DT>;
}

impl BileRegisterStreamingProducer for App {
	fn register_producer_streaming<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + ProducingNodeAPI<DT>,
	{
		self.init_resource::<FulfillerBookkeeping<DT>>()
			.register_component_as::<dyn ProducingNodeAPI<DT>, C>()
			.register_component_as::<dyn FulfillerAPI<DT>, FulfillerBookkeeping<DT>>()
			.register_dyn_param::<FulfillerParams<DT>, FulfillerSystemParams<DT>>()
	}
}
