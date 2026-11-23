use std::{collections::HashMap, marker::PhantomData};

use bevy::prelude::*;
use bevy_trait_query::{One, RegisterExt};

use crate::{DirtyState, dyn_system_params::{DynParam, RegisterDynParam}, setup::{DataType, Request}};
use crate::simple_api::{DynFulfillerParams, FulfillerParams};

// ============================ Diff Fulfiller API ============================

// The diffs are the first dirties the stored data lacks, whole ticks up to the first dirty without a diff.
// A storage that keeps them asks these next to the fulfillers, with the out of date data.
#[bevy_trait_query::queryable]
pub trait DiffFulfillerAPI<DT: DataType> {
	fn cost(
		&self,
		fulfiller_params: &mut DynFulfillerParams<DT>,
		request: &Request<DT>,
		diffs: &[DT::Diff],
	) -> Option<f32>; // None if the diffs can not be applied
	// Returns the data with the diffs applied, or None and it is passed later
	fn request(
		&mut self,
		fulfiller_params: &mut DynFulfillerParams<DT>,
		request: Request<DT>,
		data: DT::Data,
		diffs: Vec<DT::Diff>,
		last_tick: u64, // The tick of the last diff
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

// ============================ Diff Fulfiller ============================

pub struct DiffFulfillerSystemParams<DT: DataType>(PhantomData<DT>);

// The params of the producing nodes, as a node can be both. Only one of them is used at a time.
impl<DT: DataType> DynParam<FulfillerParams<DT>> for DiffFulfillerSystemParams<DT> {
	type Param = DiffApplyingNodes<'static, 'static, DT>;
}

#[derive(Resource)]
pub struct DiffFulfiller<DT: DataType> {
	applying: HashMap<DT::Params, HashMap<Entity, (u64, Option<DirtyState>)>>, // Params -> Diff applying node -> The tick of the last diff it was given, and the requested dirty state. None if that was cancelled
}

impl<DT: DataType> Default for DiffFulfiller<DT> {
	fn default() -> Self { Self { applying: HashMap::default() } }
}

impl<DT: DataType> DiffFulfiller<DT> {
	// A node that passes data later is not asked again for the same params until then, so its data is known to have
	// exactly the diffs it was given
	fn best_node(&self, diff_applying_nodes: &DiffApplyingNodes<DT>, request: &Request<DT>, diffs: &[DT::Diff]) -> Option<(f32, Entity)> {
		let applying = self.applying.get(&request.params);
		diff_applying_nodes.iter().filter(|(_, entity)| applying.is_none_or(|applying| !applying.contains_key(entity)))
			.filter_map(|(node, entity)| Some((node.diff_cost(request, diffs)?, entity))).min_by(|a, b| a.0.total_cmp(&b.0))
	}
	// Returns the tick of the last diff the node was given, or None if it was not asked for the data or that was cancelled
	pub(crate) fn applied(&mut self, source_entity: Entity, params: &DT::Params) -> Option<u64> {
		let applying = self.applying.get_mut(params)?;
		let (last_tick, wanted) = applying.remove(&source_entity)?;
		if applying.is_empty() { self.applying.remove(params); }
		wanted.and(Some(last_tick))
	}
}

impl<DT: DataType> DiffFulfillerAPI<DT> for DiffFulfiller<DT> {
	fn cost(
		&self,
		fulfiller_params: &mut DynFulfillerParams<DT>,
		request: &Request<DT>,
		diffs: &[DT::Diff],
	) -> Option<f32> {
		self.best_node(&fulfiller_params.get::<DiffFulfillerSystemParams<DT>>(), request, diffs).map(|(cost, _)| cost)
	}

	fn request(
		&mut self,
		fulfiller_params: &mut DynFulfillerParams<DT>,
		request: Request<DT>,
		mut data: DT::Data,
		diffs: Vec<DT::Diff>,
		last_tick: u64,
	) -> Option<DT::Data> {
		let mut diff_applying_nodes = fulfiller_params.get::<DiffFulfillerSystemParams<DT>>();
		let (_, entity) = self.best_node(&diff_applying_nodes, &request, &diffs)?;
		if diff_applying_nodes.get_mut(entity).expect("It was just found").0.apply(&request.params, &mut data, diffs) { return Some(data); }
		self.applying.entry(request.params).or_default().insert(entity, (last_tick, Some(request.allowed_tick)));
		None
	}

	fn cancel(
		&mut self,
		_fulfiller_params: &mut DynFulfillerParams<DT>,
		params: &DT::Params,
		latest: DirtyState,
		oldest: Option<u64>, // None if everything before latest
	) {
		// A node can not be told to stop. So it is not asked again until it passes its data, which is dropped then.
		for (_, wanted) in self.applying.get_mut(params).into_iter().flat_map(HashMap::values_mut) {
			if wanted.is_some_and(|wanted| wanted.is_cancelled_by(latest, oldest)) { *wanted = None; }
		}
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
