use bevy::{ecs::{resource::IsResource, system::SystemParam}, prelude::*};
use bevy_trait_query::{One, RegisterExt};

use crate::{DataType, DirtyState, Request, StateTick};
use crate::simple_api::{DynStorageFulfillerParams, StorageFulfillerAPI};

// ============================ Producing API ============================

#[bevy_trait_query::queryable]
pub trait ProducingNodeAPI<DT: DataType> {
	fn cost(&self, request: &Request<DT>) -> Option<f32>; // None if the node can not fulfill the request
	fn request(&mut self, request: Request<DT>);
	fn drop(&mut self, params: &DT::Params);
}

// ============================ Data Passer ============================

// TODO: Redo all this
#[derive(SystemParam)]
pub struct DataPasser<'w, 's, DT: DataType> {
	storage: Single<'w, 's, One<&'static mut dyn StorageFulfillerAPI<DT>>, With<IsResource>>,
	storage_params: DynStorageFulfillerParams<'w, 's, DT>,
}

impl<DT: DataType> DataPasser<'_, '_, DT> {
	pub fn pass_data(&mut self, _source_entity: Entity, _params: &DT::Params, _dirty_state: DirtyState, _data: DT::Data) {


		// let Ok(mut storage) = self.parts.single_mut() else { return; };
		// let mut fulfillers: Vec<_> = fulfillers.into_iter().collect();
		// // Only the fulfiller that asked the node gets the data. If it stored it, the data no longer comes from the others.
		// let Some(source) = fulfillers.iter().position(|fulfiller| fulfiller.is_source(params, source_entity)) else { return; };
		// if fulfillers[source].pass_data(&mut *storage, source_entity, params, dirty_state, data) {
		// 	for (index, fulfiller) in fulfillers.iter_mut().enumerate() {
		// 		if index != source {
		// 			fulfiller.release(&mut *storage, params);
		// 		}
		// 	}
		// }
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
	pub fn mark_dirty(&mut self, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>) {
		let Ok((mut storage, fulfillers, requesters)) = self.parts.single_mut() else { return; };
		for mut fulfiller in fulfillers.into_iter().flatten() {
			fulfiller.mark_dirty(&mut *storage, params, last_clean_tick, diff);
		}
		for mut requester in requesters.into_iter().flatten() {
			requester.mark_dirty(&mut *storage, &mut self.system_params, params, last_clean_tick, diff);
		}
	}
}

pub trait BileRegisterSimpleProducer {
	fn register_producer_simple<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + ProducingNodeAPI<DT>;
}

impl BileRegisterSimpleProducer for App {
	fn register_producer_simple<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + ProducingNodeAPI<DT>,
	{
		self.register_component_as::<dyn ProducingNodeAPI<DT>, C>()
		// .init_data_part::<DT, Fullfuller<DT, StreamStorage<DT>>>()
	}
}
