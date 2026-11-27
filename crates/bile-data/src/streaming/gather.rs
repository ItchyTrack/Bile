use std::{collections::{HashMap, HashSet}, marker::PhantomData};

use bevy::prelude::*;
use bevy_trait_query::{One, RegisterExt};

use crate::{DataType, DirtyState, Request, cancel_range::in_cancel_range, dyn_system_params::{DynParam, RegisterDynParam}};
use super::api::{DynFulfillerParams, FulfillerAPI, FulfillerParams, StreamId};
use super::producing::ProducingNodeAPI; // THIS IS VERY VERY VERY VERY VEYR BAD
// THIS SHOULD NOT DEPEND ON ANYTHING IN streaming::producing. They are NOT the same.
// This file and usages of this should stand apart from streaming::producing.
// This means it needs its own DataPasser and DataDirtier and ProducingNodeAPI.
// This means Gather nodes do not need to impl streaming::producing::ProducingNodeAPI.
// (They probably dont impl streaming::producing::ProducingNodeAPI as it would imple that they
// could produce all the data needed for the params where gather is where each not cant
// produce all the data needed for the params)
// Total fail...

// The fix is simple. Stop using streaming::producing.
// You dont need to edit streaming::producing (unless something was added just to support gather).
// Redo this as gather producing (not called that) its just a Fulfiller API impl like streaming::producing.

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
	pub(crate) fn is_streaming(&self, source_entity: Entity, stream_id: StreamId, params: &DT::Params) -> bool {
		self.streams.get(params).and_then(|streams| streams.get(&stream_id)?.1.get(&source_entity)).is_some_and(|&streaming| streaming)
	}
	// Returns if the node finished the last part of the stream, or None if it does not stream a part of it
	pub(crate) fn finish(&mut self, source_entity: Entity, stream_id: StreamId, params: &DT::Params) -> Option<bool> {
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
