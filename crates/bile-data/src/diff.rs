use crate::default::{DataType, Request};

// A node that brings data up to date with the diffs it was dirtied with instead of producing it again.
// The diffs are everything the data was dirtied with. No diff is missing, as in there never being
// a dirty without a diff inbetween 2 of them.
// The storage owns the data and the node that applied the diffs is the source of the new version of it.
// The nodes that made the versions before it are only remembered, to be dropped together with the data.
#[bevy_trait_query::queryable]
pub trait DiffApplyingNode<DT: DataType> {
	fn cost(&self, request: &Request<DT>, diffs: &[DT::Diff]) -> Option<f32>; // None if the node can not apply the diffs
	// Either edit the data and return true, or dont edit it, return false and pass the new data later. This is so that there might be async work to apply the diffs.
	fn apply(&mut self, params: &DT::Params, data: &mut DT::Data, diffs: Vec<DT::Diff>) -> bool;
	fn drop(&mut self, params: &DT::Params);
}
