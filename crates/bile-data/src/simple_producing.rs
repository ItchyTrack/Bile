use std::{collections::{BTreeSet, HashMap}, marker::PhantomData};

use bevy::{ecs::{resource::IsResource, system::SystemParam}, prelude::*};
use bevy_trait_query::{One, RegisterExt};

use crate::{btree_drain, cancel_range, DataType, DirtyState, Request, StateTick, diff::DiffFulfiller, dyn_system_params::{DynParam, RegisterDynParam}};
use crate::simple_api::{DynFulfillerParams, DynStorageFulfillerParams, FulfillerAPI, FulfillerParams, StorageFulfillerAPI};

// ============================ Producing API ============================

#[bevy_trait_query::queryable]
pub trait ProducingNodeAPI<DT: DataType> {
	fn cost(&self, request: &Request<DT>) -> Option<f32>; // None if the node can not fulfill the request
	fn request(&mut self, request: Request<DT>);
	fn cancel(&mut self, params: &DT::Params, latest: DirtyState, oldest: Option<u64>); // None if everything before latest
}

// ============================ Fulfiller ============================

pub struct FulfillerSystemParams<DT: DataType>(PhantomData<DT>);

impl<DT: DataType> DynParam<FulfillerParams<DT>> for FulfillerSystemParams<DT> {
	type Param = Query<'static, 'static, (Entity, One<&'static mut dyn ProducingNodeAPI<DT>>)>;
}

// None if no node can fulfill the request
fn best_node<DT: DataType>(producing_nodes: &Query<(Entity, One<&'static mut dyn ProducingNodeAPI<DT>>)>, request: &Request<DT>) -> Option<(f32, Entity)> {
	producing_nodes.iter().filter_map(|(entity, node)| Some((node.cost(request)?, entity))).min_by(|a, b| a.0.total_cmp(&b.0))
}

#[derive(Resource)]
pub struct FulfillerBookkeeping<DT: DataType> {
	asked: HashMap<DT::Params, HashMap<Entity, BTreeSet<DirtyState>>>, // Params -> Producing node -> Requested dirty states that were not cancelled
}

impl<DT: DataType> Default for FulfillerBookkeeping<DT> {
	fn default() -> Self { Self { asked: HashMap::default() } }
}

impl<DT: DataType> FulfillerAPI<DT> for FulfillerBookkeeping<DT> {
	fn cost(
		&self,
		fulfiller_params: &mut DynFulfillerParams<DT>,
		request: &Request<DT>,
	) -> Option<f32> {
		best_node(&fulfiller_params.get::<FulfillerSystemParams<DT>>(), request).map(|(cost, _)| cost)
	}

	fn request(
		&mut self,
		fulfiller_params: &mut DynFulfillerParams<DT>,
		request: Request<DT>,
	) {
		let mut producing_nodes = fulfiller_params.get::<FulfillerSystemParams<DT>>();
		let Some((_, entity)) = best_node(&producing_nodes, &request) else { return; };
		self.asked.entry(request.params.clone()).or_default().entry(entity).or_default().insert(request.allowed_tick);
		producing_nodes.get_mut(entity).expect("It was just found").1.request(request);
	}

	fn cancel(
		&mut self,
		fulfiller_params: &mut DynFulfillerParams<DT>,
		params: &DT::Params,
		latest: DirtyState,
		oldest: Option<u64>, // None if everything before latest
	) {
		let Some(asked) = self.asked.get_mut(params) else { return; };
		let mut producing_nodes = fulfiller_params.get::<FulfillerSystemParams<DT>>();
		asked.retain(|&entity, dirty_states| {
			if btree_drain::set_drain_range(dirty_states, cancel_range(latest, oldest)).is_empty() { return true; }
			if let Ok((_, mut node)) = producing_nodes.get_mut(entity) {
				node.cancel(params, latest, oldest);
			}
			!dirty_states.is_empty()
		});
		if asked.is_empty() { self.asked.remove(params); }
	}
}

// ============================ Data Passer ============================

#[derive(SystemParam)]
pub struct DataPasser<'w, 's, DT: DataType> {
	fulfiller: Option<Res<'w, FulfillerBookkeeping<DT>>>,
	diff_fulfiller: Option<ResMut<'w, DiffFulfiller<DT>>>,
	storage: Single<'w, 's, One<&'static mut dyn StorageFulfillerAPI<DT>>, With<IsResource>>,
	storage_params: DynStorageFulfillerParams<'w, 's, DT>,
}

impl<DT: DataType> DataPasser<'_, '_, DT> {
	pub fn pass_data(&mut self, source_entity: Entity, params: &DT::Params, dirty_state: DirtyState, data: DT::Data) {
		// Only from the nodes that were asked for the data
		let asked = self.fulfiller.as_ref().is_some_and(|fulfiller| fulfiller.asked.get(params).is_some_and(|asked| asked.contains_key(&source_entity)));
		let applied = self.diff_fulfiller.as_mut().is_some_and(|diff_fulfiller| diff_fulfiller.applied(source_entity, params, dirty_state));
		if !asked && !applied { return; }
		self.storage.pass_data(&mut self.storage_params, params, dirty_state, data);
	}
}
// ============================ Data Dirtier ============================

#[derive(SystemParam)]
pub struct DataDirtier<'w, 's, DT: DataType> {
	bile_state_tick: Res<'w, StateTick>,
	storage: Single<'w, 's, One<&'static mut dyn StorageFulfillerAPI<DT>>, With<IsResource>>,
	storage_params: DynStorageFulfillerParams<'w, 's, DT>,
}

impl<DT: DataType> DataDirtier<'_, '_, DT> {
	// The tick a leaf source dirties with, a node that passes on a dirty uses the tick it got
	pub fn current_tick(&self) -> u64 {
		self.bile_state_tick.current_tick()
	}
	pub fn mark_dirty(&mut self, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>) {
		self.storage.dirty_data(&mut self.storage_params, params, last_clean_tick, diff);
	}
}

pub trait BileRegisterSimpleProducer {
	fn register_producer_simple<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + ProducingNodeAPI<DT>;
}

impl BileRegisterSimpleProducer for App {
	fn register_producer_simple<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + ProducingNodeAPI<DT>,
	{
		self.init_resource::<FulfillerBookkeeping<DT>>()
			.register_component_as::<dyn ProducingNodeAPI<DT>, C>()
			.register_component_as::<dyn FulfillerAPI<DT>, FulfillerBookkeeping<DT>>()
			.register_dyn_param::<FulfillerParams<DT>, FulfillerSystemParams<DT>>()
	}
}
