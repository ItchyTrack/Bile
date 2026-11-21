use std::marker::PhantomData;

use crate::{DataType, DirtyState, Request, dyn_system_params::DynSystemParams};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StreamId(u64);

// ============================ Requester ============================

pub struct StorageFulfillerParams<DT: DataType>(PhantomData<DT>);

impl<DT: DataType> Default for StorageFulfillerParams<DT> {
	fn default() -> Self { Self(Default::default()) }
}

pub type DynStorageFulfillerParams<'w, 's, DT: DataType> = DynSystemParams<'w, 's, StorageFulfillerParams<DT>>;

#[bevy_trait_query::queryable]
pub trait StorageFulfillerAPI<DT: DataType> {
	fn push(&mut self,
		requester_params: &mut DynFulfillerParams<DT>,
		stream_id: StreamId,
		params: &DT::Params,
		data: DT::Data,
	);
	fn finish(&mut self,
		requester_params: &mut DynFulfillerParams<DT>,
		stream_id: StreamId,
		params: &DT::Params,
	);
	fn dirty_data(&mut self,
		requester_params: &mut DynFulfillerParams<DT>,
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
		stream_id: StreamId,
		request: Request<DT>,
	);
	fn cancel(
		&mut self,
		fulfiller_params: &mut DynFulfillerParams<DT>,
		params: &DT::Params,
	);
}

// ============================ Fulfiller ============================

pub struct StorageRequesterParams<DT: DataType>(PhantomData<DT>);

impl<DT: DataType> Default for StorageRequesterParams<DT> {
	fn default() -> Self { Self(Default::default()) }
}

pub type DynStorageRequesterParams<'w, 's, DT: DataType> = DynSystemParams<'w, 's, StorageRequesterParams<DT>>;

#[bevy_trait_query::queryable]
pub trait StorageRequesterAPI<DT: DataType> {
	fn request(
		&mut self,
		storage_params: &mut DynSystemParams<StorageRequesterParams<DT>>,
		request: Request<DT>,
	) -> StreamId;
	fn cancel(
		&mut self,
		storage_params: &mut DynSystemParams<StorageRequesterParams<DT>>,
		params: &DT::Params,
		latest: DirtyState,
		oldest: Option<u64>, // None if everything before latest
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
