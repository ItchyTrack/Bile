use std::collections::HashSet;

use bevy::prelude::*;
use bevy_trait_query::RegisterExt;

use crate::{
	default::StateTick,
	streaming::{self, Column, DataConsumingNodes, DataProducingNodes, Request, SourceNodes, StreamId, StreamingDataType},
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

// Every node that has data for the request is asked.
impl<SDT: StreamingDataType> SourceNodes<SDT> for dyn DataProducingNode<SDT> {
	fn cost(nodes: &DataProducingNodes<Self>, request: &Request<SDT>) -> Option<f32> {
		nodes.iter().filter_map(|(node, _)| node.cost(request)).reduce(|a, b| a + b)
	}
	fn request(nodes: &mut DataProducingNodes<Self>, request: Request<SDT>, stream_id: StreamId) -> HashSet<Entity> {
		let mut sources = HashSet::default();
		for (mut node, entity) in nodes.iter_mut() {
			if node.cost(&request).is_none() { continue; }
			node.request(Request { params: request.params.clone(), allowed_tick: request.allowed_tick }, stream_id);
			sources.insert(entity);
		}
		sources
	}
}

// Gather Producing Data

// The storage of streaming, only that its data is what every asked node streams and is complete when all of them finished.
pub type DataColumn<SDT> = Column<SDT, dyn DataProducingNode<SDT>>;

pub fn pass_data<SDT: StreamingDataType>(bile_state_tick: Res<StateTick>, data_column: ResMut<DataColumn<SDT>>, data_consuming_nodes: DataConsumingNodes<SDT>) {
	streaming::pass_events(bile_state_tick, data_column, data_consuming_nodes);
}

// The nodes of a gather are dropped by the same system and by the same rule as the nodes of streaming.
pub use crate::streaming::drop_data;

pub trait BileRegisterGather {
	fn register_producer_gather<SDT, C>(&mut self) -> &mut Self
	where SDT: StreamingDataType, C: Component + DataProducingNode<SDT>;
}

impl BileRegisterGather for App {
	fn register_producer_gather<SDT, C>(&mut self) -> &mut Self
	where SDT: StreamingDataType, C: Component + DataProducingNode<SDT>,
	{
		self.register_component_as::<dyn DataProducingNode<SDT>, C>();
		streaming::setup_bevy_resources::<SDT, dyn DataProducingNode<SDT>, _>(self, pass_data::<SDT>);
		self
	}
}
