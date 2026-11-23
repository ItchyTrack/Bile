use std::{collections::{BTreeSet, HashMap}, marker::PhantomData};

use bevy::prelude::*;
use bevy_trait_query::{One, RegisterExt};

use crate::{btree_drain, cancel_range, DirtyState, dyn_system_params::{DynParam, RegisterDynParam}, setup::{DataType, Request}};
use crate::simple_api::{DynFulfillerParams, FulfillerParams};

// ============================ Diff Fulfiller API ============================

// The diffs are every dirty since the data was up to date. A dirty without a diff ends the run.
// A storage that keeps them asks these next to the fulfillers, with the out of date data.
#[bevy_trait_query::queryable]
pub trait DiffFulfillerAPI<DT: DataType> {
	fn cost(
		&self,
		fulfiller_params: &mut DynFulfillerParams<DT>,
		request: &Request<DT>,
		diffs: &[DT::Diff],
	) -> Option<f32>; // None if the diffs can not be applied
	// Returns the up to date data, or None and it is passed later
	fn request(
		&mut self,
		fulfiller_params: &mut DynFulfillerParams<DT>,
		request: Request<DT>,
		data: DT::Data,
		diffs: Vec<DT::Diff>,
	) -> Option<DT::Data>;
	fn cancel(
		&mut self,
		fulfiller_params: &mut DynFulfillerParams<DT>,
		params: &DT::Params,
		latest: DirtyState,
		oldest: Option<u64>, // None if everything before latest
	);
}

// ============================ Diff Applying API ============================

#[bevy_trait_query::queryable]
pub trait DiffApplyingNode<DT: DataType> {
	fn diff_cost(&self, request: &Request<DT>, diffs: &[DT::Diff]) -> Option<f32>; // None if the node can not apply the diffs
	// Edit the data and return true, or return false and pass the new data later (async work).
	fn apply(&mut self, params: &DT::Params, data: &mut DT::Data, diffs: Vec<DT::Diff>) -> bool;
}

pub type DiffApplyingNodes<'w, 's, DT> = Query<'w, 's, (One<&'static mut dyn DiffApplyingNode<DT>>, Entity)>;

pub fn best_node<DT: DataType>(diff_applying_node: &DiffApplyingNodes<DT>, request: &Request<DT>, diffs: &[DT::Diff]) -> Option<(f32, Entity)> {
	diff_applying_node.iter().filter_map(|(node, entity)| Some((node.diff_cost(request, diffs)?, entity))).min_by(|a, b| a.0.total_cmp(&b.0))
}

// ============================ Diff Fulfiller ============================

pub struct DiffFulfillerSystemParams<DT: DataType>(PhantomData<DT>);

// The params of the producing nodes, as a node can be both. Only one of them is used at a time.
impl<DT: DataType> DynParam<FulfillerParams<DT>> for DiffFulfillerSystemParams<DT> {
	type Param = DiffApplyingNodes<'static, 'static, DT>;
}

#[derive(Resource)]
pub struct DiffFulfiller<DT: DataType> {
	applying: HashMap<DT::Params, HashMap<Entity, BTreeSet<DirtyState>>>, // Params -> Diff applying node -> Requested dirty states it passes later
}

impl<DT: DataType> Default for DiffFulfiller<DT> {
	fn default() -> Self { Self { applying: HashMap::default() } }
}

impl<DT: DataType> DiffFulfiller<DT> {
	// Returns true if the node was asked for the data
	pub(crate) fn applied(&mut self, source_entity: Entity, params: &DT::Params, dirty_state: DirtyState) -> bool {
		let Some(applying) = self.applying.get_mut(params) else { return false; };
		let Some(dirty_states) = applying.get_mut(&source_entity) else { return false; };
		dirty_states.retain(|&wanted_dirty_state| wanted_dirty_state > dirty_state);
		if dirty_states.is_empty() { applying.remove(&source_entity); }
		if applying.is_empty() { self.applying.remove(params); }
		true
	}
}

impl<DT: DataType> DiffFulfillerAPI<DT> for DiffFulfiller<DT> {
	fn cost(
		&self,
		fulfiller_params: &mut DynFulfillerParams<DT>,
		request: &Request<DT>,
		diffs: &[DT::Diff],
	) -> Option<f32> {
		best_node(&fulfiller_params.get::<DiffFulfillerSystemParams<DT>>(), request, diffs).map(|(cost, _)| cost)
	}

	fn request(
		&mut self,
		fulfiller_params: &mut DynFulfillerParams<DT>,
		request: Request<DT>,
		mut data: DT::Data,
		diffs: Vec<DT::Diff>,
	) -> Option<DT::Data> {
		let mut diff_applying_nodes = fulfiller_params.get::<DiffFulfillerSystemParams<DT>>();
		let (_, entity) = best_node(&diff_applying_nodes, &request, &diffs)?;
		if diff_applying_nodes.get_mut(entity).expect("It was just found").0.apply(&request.params, &mut data, diffs) { return Some(data); }
		self.applying.entry(request.params).or_default().entry(entity).or_default().insert(request.allowed_tick);
		None
	}

	fn cancel(
		&mut self,
		_fulfiller_params: &mut DynFulfillerParams<DT>,
		params: &DT::Params,
		latest: DirtyState,
		oldest: Option<u64>, // None if everything before latest
	) {
		let Some(applying) = self.applying.get_mut(params) else { return; };
		applying.retain(|_, dirty_states| {
			btree_drain::set_drain_range(dirty_states, cancel_range(latest, oldest));
			!dirty_states.is_empty()
		});
		if applying.is_empty() { self.applying.remove(params); }
	}
}

pub trait BileRegisterDiff {
	fn register_diff_applier<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + DiffApplyingNode<DT>;
}

impl BileRegisterDiff for App {
	fn register_diff_applier<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + DiffApplyingNode<DT>,
	{
		self.init_resource::<DiffFulfiller<DT>>()
			.register_component_as::<dyn DiffApplyingNode<DT>, C>()
			.register_component_as::<dyn DiffFulfillerAPI<DT>, DiffFulfiller<DT>>()
			.register_dyn_param::<FulfillerParams<DT>, DiffFulfillerSystemParams<DT>>()
	}
}
