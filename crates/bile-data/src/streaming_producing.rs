use std::marker::PhantomData;

use crate::{DataType, Request, dyn_system_params::DynSystemParams, streaming_consuming::StreamId};

// ============================ Producing API ============================

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
