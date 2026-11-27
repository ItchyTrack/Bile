use std::{collections::HashMap, marker::PhantomData};

use bevy::{ecs::{resource::IsResource, system::SystemParam}, prelude::*};
use bevy_trait_query::{One, RegisterExt};

use crate::{DirtyState, cancel_range::in_cancel_range, dyn_system_params::{DynParam, RegisterDynParam}, setup::{DataType, Request}};
use super::api::{DynFulfillerParams, DynStorageFulfillerParams, FulfillerParams, StorageFulfillerAPI};

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
	// Edit the data and return true, or return false and pass the new data later.
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
	this_entity: Entity,
	applying: HashMap<DT::Params, HashMap<Entity, Option<DirtyState>>>, // Params -> Diff applying node -> The requested dirty state, None if that was cancelled
}

impl<DT: DataType> DiffFulfiller<DT> {
	// A node that passes data later is not asked again for the same params until then, so its data is known to have
	// exactly the diffs it was given
	fn best_node(&self, diff_applying_nodes: &DiffApplyingNodes<DT>, request: &Request<DT>, diffs: &[DT::Diff]) -> Option<(f32, Entity)> {
		let applying = self.applying.get(&request.params);
		diff_applying_nodes.iter().filter(|(_, entity)| applying.is_none_or(|applying| !applying.contains_key(entity)))
			.filter_map(|(node, entity)| Some((node.diff_cost(request, diffs)?, entity))).min_by(|a, b| a.0.total_cmp(&b.0))
	}
	// If the node was asked for the data and that was not cancelled
	fn applied(&mut self, source_entity: Entity, params: &DT::Params) -> bool {
		let Some(applying) = self.applying.get_mut(params) else { return false; };
		let wanted = applying.remove(&source_entity).flatten();
		if applying.is_empty() { self.applying.remove(params); }
		wanted.is_some()
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
	) -> Option<DT::Data> {
		let mut diff_applying_nodes = fulfiller_params.get::<DiffFulfillerSystemParams<DT>>();
		let (_, entity) = self.best_node(&diff_applying_nodes, &request, &diffs)?;
		if diff_applying_nodes.get_mut(entity).expect("It was just found").0.apply(&request.params, &mut data, diffs) { return Some(data); }
		self.applying.entry(request.params).or_default().insert(entity, Some(request.allowed_tick));
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
		for wanted in self.applying.get_mut(params).into_iter().flat_map(HashMap::values_mut) {
			if wanted.is_some_and(|wanted| in_cancel_range(wanted, latest, oldest)) { *wanted = None; }
		}
	}
}

// ============================ Data Passer ============================

#[derive(SystemParam)]
pub struct DataPasser<'w, 's, DT: DataType> {
	diff_fulfiller: ResMut<'w, DiffFulfiller<DT>>,
	storage: Single<'w, 's, One<&'static mut dyn StorageFulfillerAPI<DT>>, With<IsResource>>,
	storage_params: DynStorageFulfillerParams<'w, 's, DT>,
}

impl<DT: DataType> DataPasser<'_, '_, DT> {
	// A diff applying node passes the data it did not apply the diffs to right away
	pub fn pass_data(&mut self, source_entity: Entity, params: &DT::Params, data: DT::Data) {
		if !self.diff_fulfiller.applied(source_entity, params) { return; }
		// The storage knows the state from the dirties it gave, this one means nothing
		self.storage.pass_data(&mut self.storage_params, self.diff_fulfiller.this_entity, params, DirtyState::Dirty { tick: 0 }, data);
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
		if !self.world().contains_resource::<DiffFulfiller<DT>>() {
			self.insert_resource(DiffFulfiller::<DT> { this_entity: Entity::PLACEHOLDER, applying: HashMap::default() });
			self.world_mut().resource_mut::<DiffFulfiller<DT>>().this_entity = self.world().resource_entity::<DiffFulfiller<DT>>().expect("It was just inserted");
		}
		self.register_component_as::<dyn DiffApplyingNode<DT>, C>()
			.register_component_as::<dyn DiffFulfillerAPI<DT>, DiffFulfiller<DT>>()
			.register_dyn_param::<FulfillerParams<DT>, DiffFulfillerSystemParams<DT>>()
	}
}
