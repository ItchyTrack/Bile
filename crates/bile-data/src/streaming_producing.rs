use std::marker::PhantomData;

use crate::{DataType, Request, dyn_system_params::DynSystemParams, streaming_consuming::StreamId};

// ============================ API ============================

pub struct StorageParams<DT: DataType>(PhantomData<DT>);

impl<DT: DataType> Default for StorageParams<DT> {
	fn default() -> Self { Self(Default::default()) }
}

pub type DynStorageParams<'w, 's, DT: DataType> = DynSystemParams<'w, 's, StorageParams<DT>>;

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

// ============================ Impl ============================

// Producing API

#[bevy_trait_query::queryable]
pub trait ProducingNodeAPI<DT: DataType> {
	fn cost(
		&self,
		request: &Request<DT>,
	) -> Option<f32>; // None if the node can not fulfill the request
	fn request(
		&mut self,
		stream_id: StreamId,
		request: Request<DT>,
	);
	fn cancel(
		&mut self,
		params: &DT::Params,
	);
}
