use crate::default::{DataType, Request};

// The diffs are every dirty since the data was made. A dirty without a diff ends the run.
// The storage owns the data. The node that applied the diffs becomes its source and the earlier
// sources are only remembered, to be dropped together with the data.
#[bevy_trait_query::queryable]
pub trait DiffApplyingNode<DT: DataType> {
	fn cost(&self, request: &Request<DT>, diffs: &[DT::Diff]) -> Option<f32>; // None if the node can not apply the diffs
	// Edit the data and return true, or return false and pass the new data later (async work).
	fn apply(&mut self, params: &DT::Params, data: &mut DT::Data, diffs: Vec<DT::Diff>) -> bool;
	fn drop(&mut self, params: &DT::Params);
}
