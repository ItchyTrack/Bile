use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use bevy_trait_query::{One, RegisterExt};

use crate::{
	parts::{BileSetDataParts, Part},
	setup::{DataType, DirtyState, Request},
	streaming::{Fulfiller, Stream, StreamId, StreamRequester, StreamRequesterStorage, StreamRequests},
};

// A gather is a stream that every node with data for the request is part of. The nodes are requested together and
// each streams only its own responsibility (only the gather step, this does not manage ownership transfer).
// It is streamed to the nodes by the stream requester.

// ============================ Nodes ============================

#[bevy_trait_query::queryable]
pub trait DataProducingNode<DT: DataType> {
	fn cost(&self, request: &Request<DT>) -> Option<f32>; // None if the node has no data for the request
	fn request(&mut self, request: Request<DT>, stream_id: StreamId);
	fn drop(&mut self, params: &DT::Params);
}

pub type DataProducingNodes<'w, 's, DT> = Query<'w, 's, (One<&'static mut dyn DataProducingNode<DT>>, Entity)>;

// ============================ Gather Fulfiller ============================

/// What the gather fulfiller needs from a storage.
pub trait GatherFulfillerStorage<DT: DataType>: Part {
	fn stream(&self, params: &DT::Params) -> Option<&Stream<DT>>;
	fn stream_mut(&mut self, params: &DT::Params) -> Option<&mut Stream<DT>>;
	/// Replaces the stream that was accepted, live or not.
	fn start(&mut self, params: &DT::Params, stream: Stream<DT>);
	fn sources(&mut self) -> &mut GatherSources<DT>;
	/// Something waits for the stream, so it is accepted when it starts.
	fn wanted(&self, params: &DT::Params) -> bool;
	/// A stream that satisfies `allowed_tick` is already on its way.
	fn is_requested(&self, params: &DT::Params, allowed_tick: &DirtyState) -> bool;
}

/// What the gather fulfiller keeps in the storage: the nodes the streams come from.
pub struct GatherSources<DT: DataType> {
	requested: HashMap<DT::Params, (StreamId, HashSet<Entity>)>, // the newest request and its nodes
	sources: HashMap<DT::Params, HashSet<Entity>>,               // the nodes of the accepted stream
	live: HashMap<DT::Params, HashSet<Entity>>,                  // the nodes of the accepted stream that did not finish
	data_to_drop: HashMap<DT::Params, HashSet<Entity>>,
}

impl<DT: DataType> Default for GatherSources<DT> {
	fn default() -> Self { Self { requested: HashMap::default(), sources: HashMap::default(), live: HashMap::default(), data_to_drop: HashMap::default() } }
}

impl<DT: DataType> GatherSources<DT> {
	/// Nothing uses or waits for the stream anymore, so the nodes it came from are dropped.
	pub fn release(&mut self, params: &DT::Params) {
		self.requested.remove(params);
		self.live.remove(params);
		if let Some(sources) = self.sources.remove(params) {
			self.data_to_drop.entry(params.clone()).or_default().extend(sources);
		}
	}
	fn is_live(&self, params: &DT::Params, source_entity: Entity) -> bool { self.live.get(params).is_some_and(|live| live.contains(&source_entity)) }
}

/// Has every node with data for the request stream its part.
#[derive(Default)]
pub struct GatherFulfiller;

impl<DT: DataType, S: GatherFulfillerStorage<DT>> Fulfiller<DT, S> for GatherFulfiller {
	type Param = DataProducingNodes<'static, 'static, DT>;

	fn fulfill(&mut self, storage: &mut S, nodes: &mut DataProducingNodes<DT>, request: Request<DT>) {
		if storage.is_requested(&request.params, &request.allowed_tick) { return; }
		let node_entities: HashSet<Entity> = nodes.iter().filter_map(|(node, entity)| node.cost(&request).and(Some(entity))).collect();
		let stream_id = StreamId::new();
		let sources = storage.sources();
		for &node_entity in &node_entities {
			if let Some(dropped) = sources.data_to_drop.get_mut(&request.params) {
				dropped.remove(&node_entity);
			}
			let mut node = nodes.get_mut(node_entity).expect("This must exist as it was found by its cost").0;
			node.request(Request::new(request.params.clone(), request.allowed_tick), stream_id);
		}
		sources.requested.insert(request.params, (stream_id, node_entities));
	}
	fn start(&mut self, storage: &mut S, source_entity: Entity, params: &DT::Params, stream_id: StreamId, dirty_state: DirtyState) -> bool {
		if let Some(stream) = storage.stream_mut(params) && stream.stream_id == stream_id {
			// another node joins the stream, which is only as fresh as its least fresh part
			if stream.dirty_state.is_fresher_than(&dirty_state) {
				stream.dirty_state = dirty_state;
			}
			return false;
		}
		let Some((_, node_entities)) = storage.sources().requested.get(params).filter(|(requested, _)| *requested == stream_id).cloned() else { return false; }; // not the newest stream
		// something fresher is already active or already finished here, or no one wants this anymore
		if storage.stream(params).is_some_and(|existing| existing.dirty_state.is_fresher_than(&dirty_state)) || !storage.wanted(params) {
			storage.sources().data_to_drop.entry(params.clone()).or_default().insert(source_entity);
			return false;
		}
		// supersede whatever was there, live or not; the nodes that are not part of the new stream are told to stop.
		// The others replaced their old work when they got the new request, a drop would stop the new stream too.
		let sources = storage.sources();
		if let Some(old_sources) = sources.sources.remove(params) {
			sources.data_to_drop.entry(params.clone()).or_default().extend(old_sources.difference(&node_entities));
		}
		sources.live.insert(params.clone(), node_entities.clone());
		sources.sources.insert(params.clone(), node_entities);
		storage.start(params, Stream::new(stream_id, dirty_state));
		true
	}
	fn push(&mut self, storage: &mut S, source_entity: Entity, params: &DT::Params, stream_id: StreamId, data: &DT::Data) -> bool {
		if !storage.sources().is_live(params, source_entity) { return false; } // stale stream, ignore
		let Some(stream) = storage.stream_mut(params).filter(|stream| stream.stream_id == stream_id) else { return false; };
		stream.data.push(data.clone());
		true
	}
	fn finish(&mut self, storage: &mut S, source_entity: Entity, params: &DT::Params, stream_id: StreamId) -> bool {
		if !storage.sources().is_live(params, source_entity) || !storage.stream(params).is_some_and(|stream| stream.stream_id == stream_id) { return false; } // stale stream, ignore
		let live = storage.sources().live.get_mut(params).expect("The node is live");
		live.remove(&source_entity);
		if !live.is_empty() { return false; }
		storage.stream_mut(params).expect("The stream is live").finished = true;
		true
	}
	fn mark_dirty(&mut self, storage: &mut S, params: &DT::Params, last_clean_tick: u64) {
		if let Some(stream) = storage.stream_mut(params) {
			stream.dirty_state = DirtyState::Dirty { tick: last_clean_tick };
		}
	}
	// The nodes of a gather are only dropped when nothing uses their stream anymore or a new stream replaced them.
	fn drop_data(&mut self, storage: &mut S, nodes: &mut DataProducingNodes<DT>) {
		if storage.sources().data_to_drop.is_empty() { return; }
		for (params, sources) in std::mem::take(&mut storage.sources().data_to_drop) {
			for source_entity in sources {
				let Ok((mut node, _)) = nodes.get_mut(source_entity) else { continue; };
				node.drop(&params);
			}
		}
	}
}

// ============================ Gather Storage ============================

/// Keeps the accepted gather of every `params`.
pub struct GatherStorage<DT: DataType> {
	streams: HashMap<DT::Params, Stream<DT>>,
	requests: StreamRequests<DT>,
	sources: GatherSources<DT>,
}

impl<DT: DataType> Default for GatherStorage<DT> {
	fn default() -> Self { Self { streams: HashMap::default(), requests: StreamRequests::default(), sources: GatherSources::default() } }
}

impl<DT: DataType> StreamRequesterStorage<DT> for GatherStorage<DT> {
	fn stream(&self, params: &DT::Params) -> Option<&Stream<DT>> { self.streams.get(params) }
	fn requests(&mut self) -> &mut StreamRequests<DT> { &mut self.requests }
	fn release(&mut self, params: &DT::Params) {
		self.streams.remove(params);
		self.sources.release(params);
	}
}

impl<DT: DataType> GatherFulfillerStorage<DT> for GatherStorage<DT> {
	fn stream(&self, params: &DT::Params) -> Option<&Stream<DT>> { self.streams.get(params) }
	fn stream_mut(&mut self, params: &DT::Params) -> Option<&mut Stream<DT>> { self.streams.get_mut(params) }
	fn start(&mut self, params: &DT::Params, stream: Stream<DT>) { self.streams.insert(params.clone(), stream); }
	fn sources(&mut self) -> &mut GatherSources<DT> { &mut self.sources }
	fn wanted(&self, params: &DT::Params) -> bool { self.requests.is_waiting(params) }
	fn is_requested(&self, params: &DT::Params, allowed_tick: &DirtyState) -> bool { self.requests.is_waiting_for(params, allowed_tick) }
}

// ============================ Registering ============================

pub trait BileRegisterGather {
	fn register_producer_gather<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + DataProducingNode<DT>;
}

impl BileRegisterGather for App {
	/// Gathers `DT` from its producers, replacing the default streaming parts. Its consumers are streaming consumers.
	fn register_producer_gather<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + DataProducingNode<DT>,
	{
		self.register_component_as::<dyn DataProducingNode<DT>, C>().set_data_parts::<DT, StreamRequester, GatherStorage<DT>, GatherFulfiller>()
	}
}
