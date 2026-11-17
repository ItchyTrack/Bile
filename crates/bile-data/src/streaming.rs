use std::{collections::{HashMap, HashSet}, marker::PhantomData};

use bevy::{
	ecs::system::{DynParamBuilder, DynSystemParam, ParamBuilder, SystemParam},
	prelude::*,
};
use bevy_trait_query::{One, RegisterExt, TraitQuery};

use crate::default::{self, ApiRequest, BileDataSchedule, BileDataSystemSet, DataRequestingAPI, DataType, DirtyState, StateTick, StreamingDataType, best, downcast};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StreamId(u64);

// The one weird thing is that the DataType kinda assumes that the data always has a mapping from
// the params. But with streaming one could image that the smaller pieces of that data does not
// match any params and instead is only a smaller piece of the params. This is a issue that does
// not need to be fixed now but its something to think about.
// Note: I think I solved this. I think there should be a new trait StreamingDataType Type which
// holds a DataType and Params. This params is used to Key a set of that data type. This allows
// something like a streaming API to not be queryable by a non streaming API, but allows the cached
// data to be used by a non streaming API. The cache is likly to look like
// StreamingDataType::Param -> {DataType::Param} and DataType::Param -> DataType::Data
// This will also allow nodes that just know which DataType Params to requst for a StreamingDataType
// Params which then requests the regulare DataType Params and passes them back like a streaming
// node would. I think that gather is very similar and gather will also use this new data type

pub struct Request<SDT: StreamingDataType> {
	pub params: SDT::Params,
	pub allowed_tick: DirtyState, // UpToDate -> Only up to date data; Dirty(tick) -> Reject data before tick
}

impl<SDT: StreamingDataType> ApiRequest for Request<SDT> {
	type Params = SDT::Params;
	type Response = ();
	fn params(&self) -> &SDT::Params { &self.params }
}

#[bevy_trait_query::queryable]
pub trait DataConsumingNode<SDT: StreamingDataType> {
	fn start_data(&mut self, params: &SDT::Params, tick: u64);
	fn push_data(&mut self, params: &SDT::Params, data_params: &<SDT::DataType as DataType>::Params, data: <SDT::DataType as DataType>::Data);
	fn finish_data(&mut self, params: &SDT::Params);
	fn dirty_data(&mut self, params: &SDT::Params, last_clean_tick: u64, diff: Option<&<SDT::DataType as DataType>::Diff>);
}

pub type DataRequester<'w, 's, SDT> = default::DataRequester<'w, 's, Request<SDT>>;

#[bevy_trait_query::queryable]
pub trait DataProducingNode<SDT: StreamingDataType> {
	fn cost(&self, request: &Request<SDT>) -> Option<f32>; // None if the node can not fulfill the request
	fn request(&mut self, request: Request<SDT>, stream_id: StreamId);
	fn drop(&mut self, params: &SDT::Params);
}

// A gather is requested from all of its nodes, anything else from the best one.
pub trait Producers<SDT: StreamingDataType>: DataProducingNode<SDT> + TraitQuery {
	const GATHER: bool;
}

impl<SDT: StreamingDataType> Producers<SDT> for dyn DataProducingNode<SDT> {
	const GATHER: bool = false;
}

type DataProducingNodes<'w, 's, P> = Query<'w, 's, (One<&'static mut P>, Entity)>;

/// Tracks whichever stream is currently accepted for a given `params`. Kept
/// around even after `finish` (with `live` empty) purely so a later, staler
/// stream has something fresher to be compared against and rejected by.
struct StreamState {
	stream_id: StreamId,
	sources: HashSet<Entity>,
	dirty_state: DirtyState,
	usages: HashSet<Entity>,
	live: HashSet<Entity>,
}

enum StreamEvent<SDT: StreamingDataType> {
	Start { entities: HashSet<Entity> },
	Push { entities: HashSet<Entity>, data_params: <SDT::DataType as DataType>::Params, data: <SDT::DataType as DataType>::Data },
	Finish { entities: HashSet<Entity> },
}

#[derive(Resource)]
pub struct DataColumn<SDT: StreamingDataType, P: ?Sized + Producers<SDT> = dyn DataProducingNode<SDT>> {
	next_stream_id: u64,
	requested: HashMap<SDT::Params, HashSet<Entity>>, // by the newest request, only read for a gather
	streams: HashMap<SDT::Params, StreamState>,
	data_to_drop: HashMap<SDT::Params, HashSet<Entity>>,
	waiting: HashMap<SDT::Params, HashMap<Entity, DirtyState>>,
	// Ordered (unlike the other modules' HashMap-based `finished`) because
	// start/push.../finish must be delivered to consumers in that order.
	events: Vec<(SDT::Params, StreamEvent<SDT>)>,
	producers: PhantomData<fn(&P)>,
}

impl<SDT: StreamingDataType, P: ?Sized + Producers<SDT>> Default for DataColumn<SDT, P> {
	fn default() -> Self {
		Self {
			next_stream_id: 0,
			requested: HashMap::default(),
			streams: HashMap::default(),
			data_to_drop: HashMap::default(),
			waiting: HashMap::default(),
			events: Vec::default(),
			producers: PhantomData,
		}
	}
}

impl<SDT: StreamingDataType, P: ?Sized + Producers<SDT>> DataRequestingAPI<Request<SDT>> for DataColumn<SDT, P> {
	fn dyn_system_param(&self) -> DynParamBuilder<'static> { DynParamBuilder::new::<DataProducingNodes<P>>(ParamBuilder) }
	fn cost(&self, dyn_system_params: &mut [DynSystemParam], request: &Request<SDT>) -> Option<f32> {
		let data_producing_node: DataProducingNodes<P> = downcast(dyn_system_params);
		let costs = data_producing_node.iter().filter_map(|(node, _)| node.cost(request));
		if P::GATHER { costs.reduce(|a, b| a + b) } else { costs.reduce(f32::min) }
	}
	fn request(&mut self, dyn_system_params: &mut [DynSystemParam], request: Request<SDT>, requester_node: Entity) {
		let already_requested = self.waiting.get(&request.params).is_some_and(|waiting| waiting.values().any(|required| required.satisfies(&request.allowed_tick)));
		// update requester
		self.waiting.entry(request.params.clone()).or_default().insert(requester_node, request.allowed_tick);
		if already_requested { return; }
		let mut data_producing_node: DataProducingNodes<P> = downcast(dyn_system_params);
		let costs = data_producing_node.iter().map(|(node, entity)| (node.cost(&request), entity));
		let sources: HashSet<Entity> = if P::GATHER { costs.filter_map(|(cost, entity)| cost.and(Some(entity))).collect() } else { best(costs).into_iter().map(|best| best.1).collect() };
		// issue a fresh stream id and kick off the stream
		self.next_stream_id += 1;
		for &node_entity in &sources {
			if let Some(dropped) = self.data_to_drop.get_mut(&request.params) {
				dropped.remove(&node_entity);
			}
			let mut node = data_producing_node.get_mut(node_entity).expect("This must exist as it was found by its cost").0;
			node.request(Request { params: request.params.clone(), allowed_tick: request.allowed_tick }, StreamId(self.next_stream_id));
		}
		self.requested.insert(request.params, sources);
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

#[derive(SystemParam)]
pub struct DataDirtier<'w, 's, SDT: StreamingDataType, P: ?Sized + Producers<SDT> = dyn DataProducingNode<SDT>> {
	bile_state_tick: Res<'w, StateTick>,
	data_column: ResMut<'w, DataColumn<SDT, P>>,
	data_producing_nodes: Query<'w, 's, One<&'static mut dyn DataConsumingNode<SDT>>>,
}

impl<'w, 's, SDT: StreamingDataType, P: ?Sized + Producers<SDT>> DataDirtier<'w, 's, SDT, P> {
	pub fn mark_dirty(&mut self, params: &SDT::Params, diff: Option<&<SDT::DataType as DataType>::Diff>) {
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
pub struct DataPasser<'w, SDT: StreamingDataType, P: ?Sized + Producers<SDT> = dyn DataProducingNode<SDT>> {
	data_column: ResMut<'w, DataColumn<SDT, P>>,
}

impl<'w, SDT: StreamingDataType, P: ?Sized + Producers<SDT>> DataPasser<'w, SDT, P> {
	/// Producer calls this once, before any `push`.
	pub fn start(&mut self, source_entity: Entity, params: &SDT::Params, stream_id: StreamId, dirty_state: DirtyState) {
		if let Some(stream) = self.data_column.streams.get_mut(params) && stream.stream_id == stream_id {
			// another node joins the gather, which is only as fresh as its least fresh part
			if stream.dirty_state.is_fresher_than(&dirty_state) {
				stream.dirty_state = dirty_state;
			}
			return;
		}

		if let Some(existing) = self.data_column.streams.get(params) && existing.dirty_state.is_fresher_than(&dirty_state) {
			// something fresher is already active or already finished here; reject outright
			self.data_column.data_to_drop.entry(params.clone()).or_default().insert(source_entity);
			return;
		}

		let Some(waiting) = self.data_column.waiting.remove(params) else {
			self.data_column.data_to_drop.entry(params.clone()).or_default().insert(source_entity);
			return; /* No one wants this anymore */
		};

		let sources = if P::GATHER { self.data_column.requested[params].clone() } else { HashSet::from([source_entity]) };
		if let Some(old) = self.data_column.streams.remove(params) {
			// supersede whatever was there, live or not; if it was still live, tell it to stop too
			let old_sources = old.sources.into_iter().filter(|old_source| !old.live.is_empty() || !sources.contains(old_source));
			self.data_column.data_to_drop.entry(params.clone()).or_default().extend(old_sources);
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

		self.data_column.streams.insert(params.clone(), StreamState { stream_id, live: sources.clone(), sources, dirty_state, usages });
	}

	/// Producer calls this any number of times between `start` and `finish`.
	pub fn push(&mut self, source_entity: Entity, params: &SDT::Params, stream_id: StreamId, data_params: <SDT::DataType as DataType>::Params, data: <SDT::DataType as DataType>::Data) {
		let Some(stream) = self.data_column.streams.get(params) else { return; };
		if stream.stream_id != stream_id || !stream.live.contains(&source_entity) { return; } // stale stream, ignore
		if stream.usages.is_empty() { return; }
		let usages = stream.usages.clone();
		self.data_column.events.push((params.clone(), StreamEvent::Push { entities: usages, data_params, data }));
	}

	/// Producer calls this once to close out the stream.
	pub fn finish(&mut self, source_entity: Entity, params: &SDT::Params, stream_id: StreamId) {
		let Some(stream) = self.data_column.streams.get_mut(params) else { return; };
		if stream.stream_id != stream_id || !stream.live.contains(&source_entity) { return; } // stale stream, ignore
		stream.live.remove(&source_entity);
		if !stream.live.is_empty() { return; }
		let entities = stream.usages.clone();
		if !entities.is_empty() {
			self.data_column.events.push((params.clone(), StreamEvent::Finish { entities }));
		}
	}
}

pub fn pass_data<SDT: StreamingDataType, P: ?Sized + Producers<SDT>>(
	bile_state_tick: Res<StateTick>,
	mut data_column: ResMut<DataColumn<SDT, P>>,
	mut data_consuming_nodes: Query<One<&'static mut dyn DataConsumingNode<SDT>>>,
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
			StreamEvent::Push { entities, data_params, data } => {
				for entity in entities {
					let Ok(mut node) = data_consuming_nodes.get_mut(entity) else { continue; };
					node.push_data(&params, &data_params, data.clone());
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

pub fn drop_data<SDT: StreamingDataType, P: ?Sized + Producers<SDT>>(
	mut data_column: ResMut<DataColumn<SDT, P>>,
	mut data_producing_nodes: Query<One<&'static mut P>>,
) {
	if data_column.data_to_drop.is_empty() { return; }

	for (params, sources) in std::mem::take(&mut data_column.data_to_drop) {
		for source_entity in sources {
			let Ok(mut node) = data_producing_nodes.get_mut(source_entity) else { continue; };
			node.drop(&params);
		}
	}
}

pub(crate) fn setup_bevy_resources<SDT: StreamingDataType, P: ?Sized + Producers<SDT>>(app: &mut App) {
	if app.world().contains_resource::<DataColumn<SDT, P>>() { return; }
	app
		.add_systems(BileDataSchedule, (
			pass_data::<SDT, P>.in_set(BileDataSystemSet::PassData),
			drop_data::<SDT, P>.in_set(BileDataSystemSet::DropData),
		))
		.init_resource::<DataColumn<SDT, P>>()
		.register_component_as::<dyn DataRequestingAPI<Request<SDT>>, DataColumn<SDT, P>>();
}

pub trait BileRegisterStreaming {
	fn register_producer_streaming<SDT, C>(&mut self) -> &mut Self
	where SDT: StreamingDataType, C: Component + DataProducingNode<SDT>;
	fn register_consumer_streaming<SDT, C>(&mut self) -> &mut Self
	where SDT: StreamingDataType, C: Component + DataConsumingNode<SDT>;
}

impl BileRegisterStreaming for App {
	fn register_producer_streaming<SDT, C>(&mut self) -> &mut Self
	where SDT: StreamingDataType, C: Component + DataProducingNode<SDT>,
	{
		self.register_component_as::<dyn DataProducingNode<SDT>, C>();
		setup_bevy_resources::<SDT, dyn DataProducingNode<SDT>>(self);
		self
	}

	fn register_consumer_streaming<SDT, C>(&mut self) -> &mut Self
	where SDT: StreamingDataType, C: Component + DataConsumingNode<SDT>,
	{
		self.register_component_as::<dyn DataConsumingNode<SDT>, C>();
		setup_bevy_resources::<SDT, dyn DataProducingNode<SDT>>(self);
		self
	}
}
