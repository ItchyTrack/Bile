use bevy::prelude::*;
use bevy_trait_query::{One, RegisterExt};

use crate::{setup::{DataType, Request}, simple::DataProducingNode};

// The diffs are every dirty since the data was made. A dirty without a diff ends the run.
// The storage owns the data. The node that applied the diffs becomes its source and the earlier
// sources are only remembered, to be dropped together with the data.
#[bevy_trait_query::queryable]
pub trait DiffApplyingNode<DT: DataType>: DataProducingNode<DT> {
	fn diff_cost(&self, request: &Request<DT>, diffs: &[DT::Diff]) -> Option<f32>; // None if the node can not apply the diffs
	// Edit the data and return true, or return false and pass the new data later (async work).
	fn apply(&mut self, params: &DT::Params, data: &mut DT::Data, diffs: Vec<DT::Diff>) -> bool;
}

pub type DiffApplyingNodes<'w, 's, DT> = Query<'w, 's, (One<&'static mut dyn DiffApplyingNode<DT>>, Entity)>;

pub fn best_node<DT: DataType>(diff_applying_node: &DiffApplyingNodes<DT>, request: &Request<DT>, diffs: &[DT::Diff]) -> Option<(f32, Entity)> {
	diff_applying_node.iter().filter_map(|(node, entity)| Some((node.diff_cost(request, diffs)?, entity))).min_by(|a, b| a.0.total_cmp(&b.0))
}

pub trait BileRegisterDiff {
	fn register_diff_applier<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + DiffApplyingNode<DT>;
}

impl BileRegisterDiff for App {
	fn register_diff_applier<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + DiffApplyingNode<DT>,
	{
		self.register_component_as::<dyn DiffApplyingNode<DT>, C>().register_component_as::<dyn DataProducingNode<DT>, C>()
	}
}
