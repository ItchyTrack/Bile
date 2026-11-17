use bevy::prelude::*;
use bevy_trait_query::RegisterExt;

use crate::{default::StreamingDataType, streaming};

// A node only streams the data that is its own resposibility. The storage requests all the nodes and gathers
// their streams into the one stream that the streaming API asked for.
// (Only the gather step. This does not manage ownership transfer)
#[bevy_trait_query::queryable]
pub trait DataProducingNode<SDT: StreamingDataType>: streaming::DataProducingNode<SDT> {}

impl<SDT: StreamingDataType> streaming::Producers<SDT> for dyn DataProducingNode<SDT> {
	const GATHER: bool = true;
}

pub type DataColumn<SDT> = streaming::DataColumn<SDT, dyn DataProducingNode<SDT>>;
pub type DataDirtier<'w, 's, SDT> = streaming::DataDirtier<'w, 's, SDT, dyn DataProducingNode<SDT>>;
pub type DataPasser<'w, SDT> = streaming::DataPasser<'w, SDT, dyn DataProducingNode<SDT>>;

pub trait BileRegisterGather {
	fn register_producer_gather<SDT, C>(&mut self) -> &mut Self
	where SDT: StreamingDataType, C: Component + DataProducingNode<SDT>;
}

impl BileRegisterGather for App {
	fn register_producer_gather<SDT, C>(&mut self) -> &mut Self
	where SDT: StreamingDataType, C: Component + DataProducingNode<SDT>,
	{
		self.register_component_as::<dyn DataProducingNode<SDT>, C>();
		streaming::setup_bevy_resources::<SDT, dyn DataProducingNode<SDT>>(self);
		self
	}
}
