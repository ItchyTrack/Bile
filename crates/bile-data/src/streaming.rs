use std::collections::{HashMap, HashSet};

use bevy::{ecs::system::SystemParam, prelude::*};
use bevy_trait_query::{One, RegisterExt};

use crate::default::{BileDataSchedule, BileDataSystemSet, DataType, DirtyState, Request, StateTick};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StreamId(u64);

#[bevy_trait_query::queryable]
pub trait DataProducingNode<DT: DataType> {
	fn cost(&self, request: &Request<DT>) -> Option<f32>; // None if the node can not fulfill the request
	fn request(&mut self, request: Request<DT>, stream_id: StreamId);
	fn drop(&mut self, params: &DT::Params);
}

#[bevy_trait_query::queryable]
pub trait DataConsumingNode<DT: DataType> {
	fn start_data(&mut self, params: &DT::Params, tick: u64);
	fn push_data(&mut self, params: &DT::Params, data: DT::Data);
	fn finish_data(&mut self, params: &DT::Params);
	fn dirty_data(&mut self, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>);
}

/// Tracks whichever stream is currently accepted for a given `params`. Kept
/// around even after `finish` (with `live = false`) purely so a later, staler
/// stream has something fresher to be compared against and rejected by.
struct StreamState {
	stream_id: StreamId,
	source_entity: Entity,
	dirty_state: DirtyState,
	usages: HashSet<Entity>,
	live: bool,
}

enum StreamEvent<DT: DataType> {
	Start { entities: HashSet<Entity> },
	Push { entities: HashSet<Entity>, data: DT::Data },
	Finish { entities: HashSet<Entity> },
}

#[derive(Resource)]
pub struct DataColumn<DT: DataType> {
	next_stream_id: u64,
	streams: HashMap<DT::Params, StreamState>,
	data_to_drop: HashMap<DT::Params, HashSet<Entity>>,
	waiting: HashMap<DT::Params, HashMap<Entity, DirtyState>>,
	// Ordered (unlike the other modules' HashMap-based `finished`) because
	// start/push.../finish must be delivered to consumers in that order.
	events: Vec<(DT::Params, StreamEvent<DT>)>,
}

impl<DT: DataType> Default for DataColumn<DT> {
	fn default() -> Self {
		Self {
			next_stream_id: 0,
			streams: HashMap::default(),
			data_to_drop: HashMap::default(),
			waiting: HashMap::default(),
			events: Vec::default(),
		}
	}
}

#[derive(SystemParam)]
pub struct DataRequester<'w, 's, DT: DataType> {
	data_column: ResMut<'w, DataColumn<DT>>,
	data_producing_node: Query<'w, 's, (One<&'static mut dyn DataProducingNode<DT>>, Entity)>,
}

impl<'w, 's, DT: DataType> DataRequester<'w, 's, DT> {
	pub fn request(&mut self, request: Request<DT>, requester_node: Entity) {
		let already_requested = self.data_column.waiting.get(&request.params).is_some_and(|waiting| waiting.values().any(|required| required.satisfies(&request.allowed_tick)));
		// update requester
		self.data_column.waiting.entry(request.params.clone()).or_default().insert(requester_node, request.allowed_tick);
		if already_requested { return; }
		// get node
		let node_entity = Self::best_node(&self.data_producing_node, &request);
		let mut node = self.data_producing_node.get_mut(node_entity).expect("This must exist as it was found in best_node").0;
		if let Some(sources) = self.data_column.data_to_drop.get_mut(&request.params) {
			sources.remove(&node_entity);
		}
		// issue a fresh stream id and kick off the stream
		self.data_column.next_stream_id += 1;
		let stream_id = StreamId(self.data_column.next_stream_id);
		node.request(request, stream_id);
	}

	pub fn cancel(&mut self, params: &DT::Params, requester_node: Entity) {
		if let Some(waiting) = self.data_column.waiting.get_mut(params) {
			waiting.remove(&requester_node);
			if waiting.is_empty() {
				self.data_column.waiting.remove(params);
			}
		}

		let Some(stream) = self.data_column.streams.get_mut(params) else { return; };
		stream.usages.remove(&requester_node);
		let source_entity = stream.source_entity;
		if stream.live && stream.usages.is_empty() && !self.data_column.waiting.contains_key(params) {
			self.data_column.data_to_drop.entry(params.clone()).or_default().insert(source_entity);
			self.data_column.streams.remove(params);
		}
	}

	fn best_node(data_producing_node: &Query<'w, 's, (One<&'static mut dyn DataProducingNode<DT>>, Entity)>, request: &Request<DT>) -> Entity {
		let mut best = (f32::MAX, None);
		for (node, entity) in data_producing_node.iter() {
			let Some(cost) = node.cost(request) else { continue; };
			if best.0 > cost {
				best.0 = cost;
				best.1 = Some(entity);
			}
		}
		let Some(entity) = best.1 else {
			panic!("Data request failed to find node to fulfill the request. This should never happen!");
		};
		entity
	}
}

#[derive(SystemParam)]
pub struct DataDirtier<'w, 's, DT: DataType> {
	bile_state_tick: Res<'w, StateTick>,
	data_column: ResMut<'w, DataColumn<DT>>,
	data_producing_nodes: Query<'w, 's, One<&'static mut dyn DataConsumingNode<DT>>>,
}

impl<'w, 's, DT: DataType> DataDirtier<'w, 's, DT> {
	pub fn mark_dirty(&mut self, params: &DT::Params, diff: Option<&DT::Diff>) {
		let Some(stream) = self.data_column.streams.get_mut(params) else { return; };
		stream.dirty_state = DirtyState::Dirty { tick: self.bile_state_tick.current_tick() - 1 };
		stream.usages.retain(|&usage| {
			let Ok(mut node) = self.data_producing_nodes.get_mut(usage) else {
				bevy::log::error!(name: "Missing DataConsumingNode", "Usage was not removed for non existent DataConsumingNode {}.", usage);
				return false;
			};
			node.dirty_data(params, self.bile_state_tick.current_tick() - 1, diff);
			true
		})
	}
}

#[derive(SystemParam)]
pub struct DataPasser<'w, DT: DataType> {
	data_column: ResMut<'w, DataColumn<DT>>,
}

impl<'w, DT: DataType> DataPasser<'w, DT> {
	/// Producer calls this once, before any `push`.
	pub fn start(&mut self, source_entity: Entity, params: &DT::Params, stream_id: StreamId, dirty_state: DirtyState) {
		if let Some(existing) = self.data_column.streams.get(params) && existing.dirty_state.is_fresher_than(&dirty_state) {
			// something fresher is already active or already finished here; reject outright
			self.data_column.data_to_drop.entry(params.clone()).or_default().insert(source_entity);
			return;
		}

		let Some(waiting) = self.data_column.waiting.remove(params) else {
			self.data_column.data_to_drop.entry(params.clone()).or_default().insert(source_entity);
			return; /* No one wants this anymore */
		};

		if let Some(old) = self.data_column.streams.remove(params) {
			// supersede whatever was there, live or not; if it was still live, tell it to stop too
			if old.live || old.source_entity != source_entity {
				self.data_column.data_to_drop.entry(params.clone()).or_default().insert(old.source_entity);
			}
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
			self.data_column.waiting.insert(params.clone(), still_waiting);
		}
		if !usages.is_empty() {
			self.data_column.events.push((params.clone(), StreamEvent::Start { entities: usages.clone() }));
		}

		self.data_column.streams.insert(params.clone(), StreamState { stream_id, source_entity, dirty_state, usages, live: true });
	}

	/// Producer calls this any number of times between `start` and `finish`.
	pub fn push(&mut self, source_entity: Entity, params: &DT::Params, stream_id: StreamId, data: DT::Data) {
		let Some(stream) = self.data_column.streams.get(params) else { return; };
		if stream.stream_id != stream_id || !stream.live || stream.source_entity != source_entity { return; } // stale stream, ignore
		if stream.usages.is_empty() { return; }
		let usages = stream.usages.clone();
		self.data_column.events.push((params.clone(), StreamEvent::Push { entities: usages, data }));
	}

	/// Producer calls this once to close out the stream.
	pub fn finish(&mut self, source_entity: Entity, params: &DT::Params, stream_id: StreamId) {
		let Some(stream) = self.data_column.streams.get_mut(params) else { return; };
		if stream.stream_id != stream_id || !stream.live || stream.source_entity != source_entity { return; } // stale stream, ignore
		stream.live = false;
		let entities = stream.usages.clone();
		if !entities.is_empty() {
			self.data_column.events.push((params.clone(), StreamEvent::Finish { entities }));
		}
	}
}

pub fn pass_data<DT: DataType>(
	bile_state_tick: Res<StateTick>,
	mut data_column: ResMut<DataColumn<DT>>,
	mut data_consuming_nodes: Query<One<&'static mut dyn DataConsumingNode<DT>>>,
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

pub fn drop_data<DT: DataType>(
	mut data_column: ResMut<DataColumn<DT>>,
	mut data_producing_nodes: Query<One<&'static mut dyn DataProducingNode<DT>>>,
) {
	if data_column.data_to_drop.is_empty() { return; }

	for (params, sources) in std::mem::take(&mut data_column.data_to_drop) {
		for source_entity in sources {
			let Ok(mut node) = data_producing_nodes.get_mut(source_entity) else { continue; };
			node.drop(&params);
		}
	}
}

fn setup_bevy_resources<DT: DataType>(app: &mut App) {
	if app.world().contains_resource::<DataColumn<DT>>() { return; }
	app
		.add_systems(BileDataSchedule, (
			pass_data::<DT>.in_set(BileDataSystemSet::PassData),
			drop_data::<DT>.in_set(BileDataSystemSet::DropData),
		))
		.init_resource::<DataColumn<DT>>();
}

pub trait BileRegisterStreaming {
	fn register_producer_streaming<DT, C>(&mut self) -> &mut Self
    where DT: DataType, C: Component + DataProducingNode<DT>;
	fn register_consumer_streaming<DT, C>(&mut self) -> &mut Self
    where DT: DataType, C: Component + DataConsumingNode<DT>;
}

impl BileRegisterStreaming for App {
    fn register_producer_streaming<DT, C>(&mut self) -> &mut Self
    where DT: DataType, C: Component + DataProducingNode<DT>,
    {
        self.register_component_as::<dyn DataProducingNode<DT>, C>();
		setup_bevy_resources::<DT>(self);
        self
    }

    fn register_consumer_streaming<DT, C>(&mut self) -> &mut Self
    where DT: DataType, C: Component + DataConsumingNode<DT>,
    {
        self.register_component_as::<dyn DataConsumingNode<DT>, C>();
		setup_bevy_resources::<DT>(self);
        self
    }
}
