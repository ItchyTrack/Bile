use std::marker::PhantomData;

use bevy::{ecs::system::SystemParam, prelude::*};
use bevy_trait_query::One;

use crate::{DataType, DirtyState, Request, dyn_system_params::DynSystemParams};

// ============================ API ============================

pub struct StorageParams<DT: DataType>(PhantomData<DT>);

impl<DT: DataType> Default for StorageParams<DT> {
	fn default() -> Self { Self(Default::default()) }
}

pub type DynStorageParams<'w, 's, DT: DataType> = DynSystemParams<'w, 's, StorageParams<DT>>;

#[bevy_trait_query::queryable]
pub trait StorageFulfillerAPI<DT: DataType> {
	fn pass_data(
		&mut self,
		storage_params: &mut DynStorageParams<DT>,
		params: &DT::Params,
		tick: u64,
		data: DT::Data,
	);
	fn dirty_data(
		&mut self,
		storage_params: &mut DynStorageParams<DT>,
		params: &DT::Params,
		last_clean_tick: u64,
		diff: Option<&DT::Diff>,
	);
}

pub struct FulfillerParams<DT: DataType>(PhantomData<DT>);

impl<DT: DataType> Default for FulfillerParams<DT> {
	fn default() -> Self { Self(Default::default()) }
}

pub type DynFulfillerParams<'w, 's, DT: DataType> = DynSystemParams<'w, 's, FulfillerParams<DT>>;

#[bevy_trait_query::queryable]
pub trait FulfillerAPI<DT: DataType> {
	fn cost(
		&self,
		fulfiller_params: &mut DynFulfillerParams<DT>,
		request: &Request<DT>,
	) -> Option<f32>; // None if the node can not fulfill the request
	fn request(
		&mut self,
		fulfiller_params: &mut DynFulfillerParams<DT>,
		request: Request<DT>,
	);
	fn drop(
		&mut self,
		fulfiller_params: &mut DynFulfillerParams<DT>,
		params: &DT::Params,
	);
}

// ============================ Impl ============================

// Producing API

#[bevy_trait_query::queryable]
pub trait ProducingNodeAPI<DT: DataType> {
	fn cost(&self, request: &Request<DT>) -> Option<f32>; // None if the node can not fulfill the request
	fn request(&mut self, request: Request<DT>);
	fn drop(&mut self, params: &DT::Params);
}

// TODO: Redo all this
#[derive(SystemParam)]
pub struct DataPasser<'w, 's, DT: DataType> {
	storage: ResMut<'w, One<&'static mut dyn StorageFulfillerAPI<DT>>>,
	storage_params: DynStorageParams<'w, 's, DT>,
}

impl<DT: DataType> DataPasser<'_, '_, DT> {
	pub fn pass_data(&mut self, source_entity: Entity, params: &DT::Params, dirty_state: DirtyState, data: DT::Data) {


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

// #[derive(SystemParam)]
// pub struct DataDirtier<'w, 's, DT: DataType> {
// 	bile_state_tick: Res<'w, StateTick>,
// 	parts: Query<
// 		'w,
// 		's,
// 		(One<&'static mut dyn DataStorage<DT>>, Option<All<&'static mut dyn ErasedFulfiller<DT>>>, Option<All<&'static mut dyn ErasedRequester<DT>>>),
// 	>,
// 	system_params: DynSystemParams<'w, 's, Dirtying<DT>>,
// }

// impl<DT: DataType> DataDirtier<'_, '_, DT> {
// 	pub fn mark_dirty(&mut self, params: &DT::Params, diff: Option<&DT::Diff>) {
// 		let last_clean_tick = self.bile_state_tick.update(); /* Was valid on the last tick */
// 		let Ok((mut storage, fulfillers, requesters)) = self.parts.single_mut() else { return; };
// 		for mut fulfiller in fulfillers.into_iter().flatten() {
// 			fulfiller.mark_dirty(&mut *storage, params, last_clean_tick, diff);
// 		}
// 		for mut requester in requesters.into_iter().flatten() {
// 			requester.mark_dirty(&mut *storage, &mut self.system_params, params, last_clean_tick, diff);
// 		}
// 	}
// }
