use bevy::prelude::*;

pub use crate::{
	DataType,
	dyn_system_params::RegisterDynParam,
	simple_consuming::{StorageRequesterAPI, RequesterAPI, ConsumingNodeAPI, RequesterSystemParams},
	simple_producing::{StorageFulfillerAPI, FulfillerAPI, ProducingNodeAPI},
};

pub trait BileRegisterSimple {
	fn register_producer_simple<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + ProducingNodeAPI<DT>;
	fn register_consumer_simple<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + ConsumingNodeAPI<DT>;
}

impl BileRegisterSimple for App {
	fn register_producer_simple<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + ProducingNodeAPI<DT>,
	{
		self.register_component_as::<dyn ProducingNodeAPI<DT>, C>()
		// .init_data_part::<DT, Fullfuller<DT, StreamStorage<DT>>>()
	}

	fn register_consumer_simple<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + ConsumingNodeAPI<DT>,
	{
		self.register_component_as::<dyn ConsumingNodeAPI<DT>, C>().register_dyn_param::<RequesterSystemParams>()
		// .init_data_part::<DT, StreamRequester<DT, StreamStorage<DT>>>()
	}
}
