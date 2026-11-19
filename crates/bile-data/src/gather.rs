use std::{
	collections::{HashMap, HashSet},
	marker::PhantomData,
};

use bevy::prelude::*;
use bevy_trait_query::{One, RegisterExt};

use crate::{
	parts::{BileDataParts, DataPart},
	setup::{DataType, DirtyState, Request},
	streaming::{Fulfiller, Stream, StreamFulfillerStorage, StreamId, StreamStorage, setup_fulfiller},
};

// A gather is a stream that every node with data for the request is part of. The nodes are requested together and
// each streams only its own responsibility (only the gather step, this does not manage ownership transfer).
// It is a streaming fulfiller, so it keeps its streams in the storage of the streaming data type next to the others.

// ============================ Nodes ============================

#[bevy_trait_query::queryable]
pub trait DataProducingNode<DT: DataType> {
	fn cost(&self, request: &Request<DT>) -> Option<f32>; // None if the node has no data for the request
	fn request(&mut self, request: Request<DT>, stream_id: StreamId);
	fn drop(&mut self, params: &DT::Params);
}

pub type DataProducingNodes<'w, 's, DT> = Query<'w, 's, (One<&'static mut dyn DataProducingNode<DT>>, Entity)>;

// ============================ Gather Fulfiller ============================

/// Has every node with data for the request stream its part. It costs as much as the most expensive of them.
#[derive(Component)]
pub struct GatherFulfiller<DT: DataType, S> {
	requested: HashMap<DT::Params, (StreamId, HashSet<Entity>)>, // the newest request and its nodes
	sources: HashMap<DT::Params, HashSet<Entity>>,               // the nodes of the accepted stream
	live: HashMap<DT::Params, HashSet<Entity>>,                  // the nodes of the accepted stream that did not finish
	data_to_drop: HashMap<DT::Params, HashSet<Entity>>,
	_marker: PhantomData<fn() -> S>,
}

impl<DT: DataType, S> Default for GatherFulfiller<DT, S> {
	fn default() -> Self {
		Self { requested: HashMap::default(), sources: HashMap::default(), live: HashMap::default(), data_to_drop: HashMap::default(), _marker: PhantomData }
	}
}

impl<DT: DataType, S: StreamFulfillerStorage<DT>> GatherFulfiller<DT, S> {
	fn is_live(&self, params: &DT::Params, source_entity: Entity) -> bool { self.live.get(params).is_some_and(|live| live.contains(&source_entity)) }
}

impl<DT: DataType, S: StreamFulfillerStorage<DT>> DataPart<DT> for GatherFulfiller<DT, S> {
	type Storage = S;

	fn setup(app: &mut App) { setup_fulfiller::<DT, Self>(app); }
}

impl<DT: DataType, S: StreamFulfillerStorage<DT>> Fulfiller<DT> for GatherFulfiller<DT, S> {
	type Param = DataProducingNodes<'static, 'static, DT>;

	fn cost(&self, _storage: &S, nodes: &mut DataProducingNodes<DT>, request: &Request<DT>) -> Option<f32> {
		nodes.iter().filter_map(|(node, _)| node.cost(request)).reduce(f32::max)
	}
	fn fulfill(&mut self, storage: &mut S, nodes: &mut DataProducingNodes<DT>, request: Request<DT>) {
		if storage.is_requested(&request.params, &request.allowed_tick) { return; }
		let node_entities: HashSet<Entity> = nodes.iter().filter_map(|(node, entity)| node.cost(&request).and(Some(entity))).collect();
		let stream_id = StreamId::new();
		for &node_entity in &node_entities {
			if let Some(dropped) = self.data_to_drop.get_mut(&request.params) {
				dropped.remove(&node_entity);
			}
			let mut node = nodes.get_mut(node_entity).expect("This must exist as it was found by its cost").0;
			node.request(Request::new(request.params.clone(), request.allowed_tick), stream_id);
		}
		self.requested.insert(request.params, (stream_id, node_entities));
	}
	fn start(&mut self, storage: &mut S, source_entity: Entity, params: &DT::Params, stream_id: StreamId, dirty_state: DirtyState) -> bool {
		if let Some(stream) = storage.stream_mut(params) && stream.stream_id == stream_id {
			// another node joins the stream, which is only as fresh as its least fresh part
			if stream.dirty_state.is_fresher_than(&dirty_state) {
				stream.dirty_state = dirty_state;
			}
			return false;
		}
		let Some((_, node_entities)) = self.requested.get(params).filter(|(requested, _)| *requested == stream_id).cloned() else { return false; }; // not the newest stream
		// something fresher is already active or already finished here, or no one wants this anymore
		if storage.stream(params).is_some_and(|existing| existing.dirty_state.is_fresher_than(&dirty_state)) || !storage.wanted(params) {
			self.data_to_drop.entry(params.clone()).or_default().insert(source_entity);
			return false;
		}
		// supersede whatever was there, live or not; the nodes that are not part of the new stream are told to stop.
		// The others replaced their old work when they got the new request, a drop would stop the new stream too.
		if let Some(old_sources) = self.sources.remove(params) {
			self.data_to_drop.entry(params.clone()).or_default().extend(old_sources.difference(&node_entities));
		}
		self.live.insert(params.clone(), node_entities.clone());
		self.sources.insert(params.clone(), node_entities);
		storage.start(params, Stream::new(stream_id, dirty_state));
		true
	}
	fn push(&mut self, storage: &mut S, source_entity: Entity, params: &DT::Params, stream_id: StreamId, data: &DT::Data) -> bool {
		if !self.is_live(params, source_entity) || !storage.stream(params).is_some_and(|stream| stream.stream_id == stream_id) { return false; } // stale stream, ignore
		storage.push(params, data);
		true
	}
	fn finish(&mut self, storage: &mut S, source_entity: Entity, params: &DT::Params, stream_id: StreamId) -> bool {
		if !self.is_live(params, source_entity) || !storage.stream(params).is_some_and(|stream| stream.stream_id == stream_id) { return false; } // stale stream, ignore
		let live = self.live.get_mut(params).expect("The node is live");
		live.remove(&source_entity);
		if !live.is_empty() { return false; }
		storage.finish(params);
		true
	}
	fn mark_dirty(&mut self, storage: &mut S, params: &DT::Params, last_clean_tick: u64) {
		if let Some(stream) = storage.stream_mut(params) {
			stream.dirty_state = DirtyState::Dirty { tick: last_clean_tick };
		}
	}
	fn release(&mut self, _storage: &mut S, params: &DT::Params) {
		self.requested.remove(params);
		self.live.remove(params);
		if let Some(sources) = self.sources.remove(params) {
			self.data_to_drop.entry(params.clone()).or_default().extend(sources);
		}
	}
	// The nodes of a gather are only dropped when nothing uses their stream anymore or a new stream replaced them.
	fn drop_data(&mut self, _storage: &S, nodes: &mut DataProducingNodes<DT>) {
		if self.data_to_drop.is_empty() { return; }
		for (params, sources) in std::mem::take(&mut self.data_to_drop) {
			for source_entity in sources {
				let Ok((mut node, _)) = nodes.get_mut(source_entity) else { continue; };
				node.drop(&params);
			}
		}
	}
}

// ============================ Registering ============================

pub trait BileRegisterGather {
	fn register_producer_gather<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + DataProducingNode<DT>;
}

impl BileRegisterGather for App {
	fn register_producer_gather<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + DataProducingNode<DT>,
	{
		self.register_component_as::<dyn DataProducingNode<DT>, C>().init_data_part::<DT, GatherFulfiller<DT, StreamStorage<DT>>>()
	}
}
