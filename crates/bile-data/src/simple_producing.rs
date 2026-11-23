use std::{collections::{BTreeSet, HashMap}, marker::PhantomData};

use bevy::{ecs::{resource::IsResource, system::SystemParam}, prelude::*};
use bevy_trait_query::{One, RegisterExt};

use crate::{btree_drain, cancel_range::{cancel_range, is_cancelled, unheld_ranges}, DataType, DirtyState, Request, StateTick, diff::DiffFulfiller, dyn_system_params::{DynParam, RegisterDynParam}};
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

// The dirty states that were requested and no data satisfied yet, and the freshest of the dirty states the last data satisfied
type Asked = (BTreeSet<DirtyState>, Option<DirtyState>);

#[derive(Resource)]
pub struct FulfillerBookkeeping<DT: DataType> {
	asked: HashMap<DT::Params, (Asked, HashMap<Entity, Asked>)>, // Params -> What the storage asked for, Producing node -> What it was asked for
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
		let (storage, nodes) = self.asked.entry(request.params.clone()).or_default();
		storage.0.insert(request.allowed_tick);
		nodes.entry(entity).or_default().0.insert(request.allowed_tick);
		producing_nodes.get_mut(entity).expect("It was just found").1.request(request);
	}

	fn cancel(
		&mut self,
		fulfiller_params: &mut DynFulfillerParams<DT>,
		params: &DT::Params,
		latest: DirtyState,
		oldest: Option<u64>, // None if everything before latest
	) {
		let Some(((waiting, used), nodes)) = self.asked.get_mut(params) else { return; };
		btree_drain::set_drain_range(waiting, cancel_range(latest, oldest));
		if used.is_some_and(|used| is_cancelled(used, latest, oldest)) { *used = None; }
		let mut producing_nodes = fulfiller_params.get::<FulfillerSystemParams<DT>>();
		// The storage keeps one dirty state for the data of every node. So a node keeps what its data satisfied until
		// the storage asks for nothing here anymore, then every node is told to stop.
		if waiting.is_empty() && used.is_none() {
			for &entity in nodes.keys() {
				if let Ok((_, mut node)) = producing_nodes.get_mut(entity) { node.cancel(params, DirtyState::UpToDate, None); }
			}
			self.asked.remove(params);
			return;
		}
		nodes.retain(|&entity, (waiting, used)| {
			for (latest, oldest) in unheld_ranges(*used, latest, oldest) {
				if !btree_drain::set_drain_range(waiting, cancel_range(latest, oldest)).is_empty() && let Ok((_, mut node)) = producing_nodes.get_mut(entity) {
					node.cancel(params, latest, oldest);
				}
			}
			!waiting.is_empty() || used.is_some()
		});
	}
}

// ============================ Data Passer ============================

#[derive(SystemParam)]
pub struct DataPasser<'w, 's, DT: DataType> {
	fulfiller: Option<ResMut<'w, FulfillerBookkeeping<DT>>>,
	diff_fulfiller: Option<ResMut<'w, DiffFulfiller<DT>>>,
	storage: Single<'w, 's, One<&'static mut dyn StorageFulfillerAPI<DT>>, With<IsResource>>,
	storage_params: DynStorageFulfillerParams<'w, 's, DT>,
}

impl<DT: DataType> DataPasser<'_, '_, DT> {
	pub fn pass_data(&mut self, source_entity: Entity, params: &DT::Params, dirty_state: DirtyState, data: DT::Data) {
		// Only from the nodes that were asked for the data
		let Some((storage, nodes)) = self.fulfiller.as_mut().and_then(|fulfiller| fulfiller.asked.get_mut(params)) else { return; };
		let Some(node) = nodes.get_mut(&source_entity) else { return; };
		// The freshest of the requests the data satisfies is what the data is kept for
		for (waiting, used) in [node, storage] {
			if let Some(&wanted) = btree_drain::set_drain_range(waiting, ..=dirty_state).last() { *used = Some(wanted); }
		}
		self.storage.pass_data(&mut self.storage_params, params, dirty_state, data);
	}
	// A diff applying node passes the data it did not apply the diffs to right away
	pub fn pass_diffed_data(&mut self, source_entity: Entity, params: &DT::Params, data: DT::Data) {
		let Some(last_tick) = self.diff_fulfiller.as_mut().and_then(|diff_fulfiller| diff_fulfiller.applied(source_entity, params)) else { return; };
		// It has every dirty up to the tick of the last diff, as all the dirties of a tick come before any work of that tick
		self.storage.pass_data(&mut self.storage_params, params, DirtyState::Dirty { tick: last_tick + 1 }, data);
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
