use std::collections::{HashMap, HashSet};

use bevy::{
	ecs::system::{DynParamBuilder, DynSystemParam, ParamBuilder},
	prelude::*,
};
use bevy_trait_query::{One, RegisterExt};

use crate::{
	default::{BileDataSchedule, BileDataSystemSet, DirtyState, StateTick},
	dyn_system_params::downcast,
	streaming::{DataConsumingNodes, DataRequestingAPI, DataStorage, Request, StreamId, StreamingDataType},
};

// Gather Producing

// Every node is requested and streams only its own resposibility.
// (Only the gather step. This does not manage ownership transfer)
#[bevy_trait_query::queryable]
pub trait DataProducingNode<SDT: StreamingDataType> {
	fn cost(&self, request: &Request<SDT>) -> Option<f32>; // None if the node has no data for the request
	fn request(&mut self, request: Request<SDT>, stream_id: StreamId);
	fn drop(&mut self, params: &SDT::Params);
}

type DataProducingNodes<'w, 's, SDT> = Query<'w, 's, (One<&'static mut dyn DataProducingNode<SDT>>, Entity)>;

// Gather Producing Data

/// Tracks whichever stream is currently accepted for a given `params`. Kept
/// around even after `finish` (with `live` empty) purely so a later, staler
/// stream has something fresher to be compared against and rejected by.
struct StreamState<SDT: StreamingDataType> {
	stream_id: StreamId,
	sources: HashSet<Entity>,
	dirty_state: DirtyState,
	usages: HashSet<Entity>,
	data: Vec<SDT::Data>,
	live: HashSet<Entity>,
}

enum StreamEvent<SDT: StreamingDataType> {
	Start { entities: HashSet<Entity> },
	Push { entities: HashSet<Entity>, data: SDT::Data },
	Finish { entities: HashSet<Entity> },
}

#[derive(Resource)]
pub struct DataColumn<SDT: StreamingDataType> {
	requested: HashMap<SDT::Params, (StreamId, HashSet<Entity>)>, // the newest request and its nodes
	streams: HashMap<SDT::Params, StreamState<SDT>>,
	data_to_drop: HashMap<SDT::Params, HashSet<Entity>>,
	waiting: HashMap<SDT::Params, HashMap<Entity, DirtyState>>,
	// Ordered (unlike the other modules' HashMap-based `finished`) because
	// start/push.../finish must be delivered to consumers in that order.
	events: Vec<(SDT::Params, StreamEvent<SDT>)>,
}

impl<SDT: StreamingDataType> Default for DataColumn<SDT> {
	fn default() -> Self {
		Self {
			requested: HashMap::default(),
			streams: HashMap::default(),
			data_to_drop: HashMap::default(),
			waiting: HashMap::default(),
			events: Vec::default(),
		}
	}
}

impl<SDT: StreamingDataType> DataRequestingAPI<SDT> for DataColumn<SDT> {
	fn dyn_system_param(&self) -> DynParamBuilder<'static> { DynParamBuilder::new::<DataProducingNodes<SDT>>(ParamBuilder) }
	fn cost(&self, dyn_system_params: &mut [DynSystemParam], request: &Request<SDT>) -> Option<f32> {
		let data_producing_node: DataProducingNodes<SDT> = downcast(dyn_system_params);
		data_producing_node.iter().filter_map(|(node, _)| node.cost(request)).reduce(|a, b| a + b)
	}
	fn request(&mut self, dyn_system_params: &mut [DynSystemParam], request: Request<SDT>, requester_node: Entity) {
		if let Some(stream) = self.streams.get_mut(&request.params) && stream.dirty_state.satisfies(&request.allowed_tick) {
			stream.usages.insert(requester_node);
			let entities = HashSet::from([requester_node]);
			self.events.push((request.params.clone(), StreamEvent::Start { entities: entities.clone() }));
			self.events.extend(stream.data.iter().map(|data| (request.params.clone(), StreamEvent::Push { entities: entities.clone(), data: data.clone() })));
			if stream.live.is_empty() {
				self.events.push((request.params, StreamEvent::Finish { entities }));
			}
			return;
		}
		let already_requested = self.waiting.get(&request.params).is_some_and(|waiting| waiting.values().any(|required| required.satisfies(&request.allowed_tick)));
		// update requester
		self.waiting.entry(request.params.clone()).or_default().insert(requester_node, request.allowed_tick);
		if already_requested { return; }
		let mut data_producing_node: DataProducingNodes<SDT> = downcast(dyn_system_params);
		let sources: HashSet<Entity> = data_producing_node.iter().filter_map(|(node, entity)| node.cost(&request).and(Some(entity))).collect();
		// issue a fresh stream id and kick off the stream
		let stream_id = StreamId::new();
		for &node_entity in &sources {
			if let Some(dropped) = self.data_to_drop.get_mut(&request.params) {
				dropped.remove(&node_entity);
			}
			let mut node = data_producing_node.get_mut(node_entity).expect("This must exist as it was found by its cost").0;
			node.request(Request { params: request.params.clone(), allowed_tick: request.allowed_tick }, stream_id);
		}
		self.requested.insert(request.params, (stream_id, sources));
	}
	fn cancel(&mut self, _dyn_system_params: &mut [DynSystemParam], params: &SDT::Params, requester_node: Entity) {
		if let Some(waiting) = self.waiting.get_mut(params) {
			waiting.remove(&requester_node);
			if waiting.is_empty() {
				self.waiting.remove(params);
			}
		}

		let Some(stream) = self.streams.get_mut(params) else {
			if !self.waiting.contains_key(params) { self.requested.remove(params); }
			return;
		};
		stream.usages.remove(&requester_node);
		if !stream.live.is_empty() && stream.usages.is_empty() && !self.waiting.contains_key(params) {
			self.data_to_drop.entry(params.clone()).or_default().extend(std::mem::take(&mut stream.sources));
			self.streams.remove(params);
			self.requested.remove(params);
		}
	}
}

impl<SDT: StreamingDataType> DataStorage<SDT> for DataColumn<SDT> {
	fn start(&mut self, source_entity: Entity, params: &SDT::Params, stream_id: StreamId, dirty_state: DirtyState) {
		if let Some(stream) = self.streams.get_mut(params) && stream.stream_id == stream_id {
			// another node joins the stream, which is only as fresh as its least fresh part
			if stream.dirty_state.is_fresher_than(&dirty_state) {
				stream.dirty_state = dirty_state;
			}
			return;
		}

		let Some(sources) = self.requested.get(params).filter(|requested| requested.0 == stream_id).map(|requested| requested.1.clone()) else { return; }; // the stream of another storage
		if let Some(existing) = self.streams.get(params) && existing.dirty_state.is_fresher_than(&dirty_state) {
			// something fresher is already active or already finished here; reject outright
			self.data_to_drop.entry(params.clone()).or_default().insert(source_entity);
			return;
		}

		let Some(waiting) = self.waiting.remove(params) else {
			self.data_to_drop.entry(params.clone()).or_default().insert(source_entity);
			return; /* No one wants this anymore */
		};

		if let Some(old) = self.streams.remove(params) {
			// supersede whatever was there, live or not; if it was still live, tell it to stop too
			let old_sources = old.sources.into_iter().filter(|old_source| !old.live.is_empty() || !sources.contains(old_source));
			self.data_to_drop.entry(params.clone()).or_default().extend(old_sources);
		}

		let mut usages = HashSet::default();
		let mut still_waiting = HashMap::default();
		for (entity, required) in waiting {
			if dirty_state.satisfies(&required) {
				usages.insert(entity);
			} else {
				still_waiting.insert(entity, required);
			}
		}
		if !still_waiting.is_empty() {
			self.waiting.insert(params.clone(), still_waiting);
		}
		if !usages.is_empty() {
			self.events.push((params.clone(), StreamEvent::Start { entities: usages.clone() }));
		}

		self.streams.insert(params.clone(), StreamState { stream_id, live: sources.clone(), sources, dirty_state, usages, data: Vec::new() });
	}
	fn push(&mut self, source_entity: Entity, params: &SDT::Params, stream_id: StreamId, data: &SDT::Data) {
		let Some(stream) = self.streams.get_mut(params) else { return; };
		if stream.stream_id != stream_id || !stream.live.contains(&source_entity) { return; } // stale stream, ignore
		stream.data.push(data.clone());
		if stream.usages.is_empty() { return; }
		let usages = stream.usages.clone();
		self.events.push((params.clone(), StreamEvent::Push { entities: usages, data: data.clone() }));
	}
	fn finish(&mut self, source_entity: Entity, params: &SDT::Params, stream_id: StreamId) {
		let Some(stream) = self.streams.get_mut(params) else { return; };
		if stream.stream_id != stream_id || !stream.live.contains(&source_entity) { return; } // stale stream, ignore
		stream.live.remove(&source_entity);
		if !stream.live.is_empty() { return; }
		let entities = stream.usages.clone();
		if !entities.is_empty() {
			self.events.push((params.clone(), StreamEvent::Finish { entities }));
		}
	}
	fn dirty_data(&mut self, data_consuming_nodes: &mut DataConsumingNodes<SDT>, params: &SDT::Params, last_clean_tick: u64, diff: Option<&SDT::Diff>) {
		let Some(stream) = self.streams.get_mut(params) else { return; };
		stream.dirty_state = DirtyState::Dirty { tick: last_clean_tick };
		stream.usages.retain(|&usage| {
			let Ok(mut node) = data_consuming_nodes.get_mut(usage) else {
				bevy::log::error!(name: "Missing DataConsumingNode", "Usage was not removed for non existent DataConsumingNode {}.", usage);
				return false;
			};
			node.dirty_data(params, last_clean_tick, diff);
			true
		})
	}
	// The nodes of a gather are not streaming nodes, drop_data here drops them.
	fn uses(&self, _params: &SDT::Params, _source_entity: Entity) -> bool { false }
	fn drop_data(&mut self) -> HashMap<SDT::Params, HashSet<Entity>> { HashMap::default() }
}

pub fn pass_data<SDT: StreamingDataType>(
	bile_state_tick: Res<StateTick>,
	mut data_column: ResMut<DataColumn<SDT>>,
	mut data_consuming_nodes: DataConsumingNodes<SDT>,
) {
	if data_column.events.is_empty() { return; }

	for (params, event) in std::mem::take(&mut data_column.events) {
		match event {
			StreamEvent::Start { entities } => {
				let tick = match data_column.streams.get(&params).map(|s| s.dirty_state) {
					Some(DirtyState::Dirty { tick }) => tick,
					_ => bile_state_tick.current_tick(),
				};
				for entity in entities {
					let Ok(mut node) = data_consuming_nodes.get_mut(entity) else { continue; };
					node.start_data(&params, tick);
				}
			}
			StreamEvent::Push { entities, data } => {
				for entity in entities {
					let Ok(mut node) = data_consuming_nodes.get_mut(entity) else { continue; };
					node.push_data(&params, data.clone());
				}
			}
			StreamEvent::Finish { entities } => {
				for entity in entities {
					let Ok(mut node) = data_consuming_nodes.get_mut(entity) else { continue; };
					node.finish_data(&params);
				}
			}
		}
	}
}

pub fn drop_data<SDT: StreamingDataType>(
	mut data_column: ResMut<DataColumn<SDT>>,
	mut data_producing_nodes: Query<One<&'static mut dyn DataProducingNode<SDT>>>,
) {
	if data_column.data_to_drop.is_empty() { return; }

	for (params, sources) in std::mem::take(&mut data_column.data_to_drop) {
		for source_entity in sources {
			let Ok(mut node) = data_producing_nodes.get_mut(source_entity) else { continue; };
			node.drop(&params);
		}
	}
}

fn setup_bevy_resources<SDT: StreamingDataType>(app: &mut App) {
	if app.world().contains_resource::<DataColumn<SDT>>() { return; }
	app
		.add_systems(BileDataSchedule, (
			pass_data::<SDT>.in_set(BileDataSystemSet::PassData),
			drop_data::<SDT>.in_set(BileDataSystemSet::DropData),
		))
		.init_resource::<DataColumn<SDT>>()
		.register_component_as::<dyn DataRequestingAPI<SDT>, DataColumn<SDT>>()
		.register_component_as::<dyn DataStorage<SDT>, DataColumn<SDT>>();
}

pub trait BileRegisterGather {
	fn register_producer_gather<SDT, C>(&mut self) -> &mut Self
	where SDT: StreamingDataType, C: Component + DataProducingNode<SDT>;
}

impl BileRegisterGather for App {
	fn register_producer_gather<SDT, C>(&mut self) -> &mut Self
	where SDT: StreamingDataType, C: Component + DataProducingNode<SDT>,
	{
		self.register_component_as::<dyn DataProducingNode<SDT>, C>();
		setup_bevy_resources::<SDT>(self);
		self
	}
}
