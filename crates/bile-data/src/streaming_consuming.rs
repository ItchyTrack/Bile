use std::marker::PhantomData;

use bevy::{ecs::system::SystemParam, prelude::*};
use bevy_trait_query::One;

use crate::{DataType, Request, dyn_system_params::DynSystemParams};

// ============================ API ============================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StreamId(u64);

pub struct StorageParams<DT: DataType>(PhantomData<DT>);

impl<DT: DataType> Default for StorageParams<DT> {
	fn default() -> Self { Self(Default::default()) }
}

pub type DynStorageParams<'w, 's, DT: DataType> = DynSystemParams<'w, 's, StorageParams<DT>>;

#[bevy_trait_query::queryable]
pub trait StorageRequesterAPI<DT: DataType> {
	fn request(
		&mut self,
		storage_params: &mut DynSystemParams<StorageParams<DT>>,
		request: Request<DT>,
	) -> StreamId;
	fn cancel(
		&mut self,
		storage_params: &mut DynSystemParams<StorageParams<DT>>,
		params: &DT::Params,
	);
}

pub struct RequesterParams<DT: DataType>(PhantomData<DT>);

impl<DT: DataType> Default for RequesterParams<DT> {
	fn default() -> Self { Self(Default::default()) }
}

pub type DynRequesterParams<'w, 's, DT: DataType> = DynSystemParams<'w, 's, RequesterParams<DT>>;

#[bevy_trait_query::queryable]
pub trait RequesterAPI<DT: DataType> {
	fn push(&mut self,
		requester_params: &mut DynRequesterParams<DT>,
		stream_id: StreamId,
		params: &DT::Params,
		data: DT::Data,
	);
	fn finish(&mut self,
		requester_params: &mut DynRequesterParams<DT>,
		stream_id: StreamId,
		params: &DT::Params,
	);
	fn dirty_data(&mut self,
		requester_params: &mut DynRequesterParams<DT>,
		params: &DT::Params,
		last_clean_tick: u64,
		diff: Option<&DT::Diff>,
	);
}

// ============================ Impl ============================

// Consuming API

#[bevy_trait_query::queryable]
pub trait ConsumingNodeAPI<DT: DataType> {
	fn push_data(&mut self, stream_id: StreamId, params: &DT::Params, data: DT::Data);
	fn finish_data(&mut self, stream_id: StreamId, params: &DT::Params);
	fn dirty_data(&mut self, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>);
}

#[derive(SystemParam)]
pub struct Requester<'w, 's, DT: DataType> {
	requester: ResMut<'w, RequesterBookkeeping<DT>>,
	storage: ResMut<'w, One<&'static mut dyn StorageRequesterAPI<DT>>>,
	storage_params: DynStorageParams<'w, 's, DT>,
}

pub struct RequesterBookkeeping<DT: DataType> { }

impl<DT: DataType> RequesterAPI<DT> for RequesterBookkeeping<DT> {
	fn push(&mut self,
		requester_params: &mut DynRequesterParams<DT>,
		stream_id: StreamId,
		params: &DT::Params,
		data: DT::Data,
	) { todo!() }
	fn finish(&mut self,
		requester_params: &mut DynRequesterParams<DT>,
		stream_id: StreamId,
		params: &DT::Params,
	) { todo!() }
	fn dirty_data(&mut self,
		requester_params: &mut DynRequesterParams<DT>,
		params: &DT::Params,
		last_clean_tick: u64,
		diff: Option<&DT::Diff>,
	) { todo!() }
}

impl<DT: DataType> Requester<'_, '_, DT> {
	pub fn request(&mut self, request: Request<DT>, requester_node: Entity) -> StreamId {
		self.storage.request(request)
		// TODO: add bookkeeping using `waiting`, `finished`, `usages`.
	}
	pub fn cancel(&mut self, params: &DT::Params /* maybe StreamId? */, requester_node: Entity) {
		self.storage.cancel(params);
		// TODO: update bookkeeping using `waiting`, `finished`, `usages`.
	}
}
