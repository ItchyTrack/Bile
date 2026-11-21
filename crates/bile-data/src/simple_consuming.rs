use std::{collections::{BTreeMap, BTreeSet, HashMap, HashSet}, marker::PhantomData};

use bevy::{ecs::{resource::IsResource, system::SystemParam}, prelude::*};
use bevy_trait_query::{One, RegisterExt};

use crate::{btree_drain, DataType, DirtyState, Request, dyn_system_params::{DynParam, RegisterDynParam}};
use crate::simple_api::{DynRequesterParams, DynStorageRequesterParams, RequesterAPI, RequesterParams, StorageRequesterAPI};

// ============================ Consuming API ============================

#[bevy_trait_query::queryable]
pub trait ConsumingNodeAPI<DT: DataType> {
	fn pass_data(&mut self, params: &DT::Params, dirty_state: DirtyState, data: &DT::Data);
	fn dirty_data(&mut self, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>);
}

// ============================ Requester ============================

pub struct RequesterSystemParams<DT: DataType>(PhantomData<DT>);

impl<DT: DataType> DynParam<RequesterParams<DT>> for RequesterSystemParams<DT> {
	type Param = Query<'static, 'static, One<&'static mut dyn ConsumingNodeAPI<DT>>>;
}

#[derive(Resource)]
pub struct RequesterBookkeeping<DT: DataType> {
	waiting: HashMap<DT::Params, (HashMap<Entity, BTreeSet<DirtyState>>, BTreeMap<DirtyState, HashSet<Entity>>)>,
	usages: HashMap<DT::Params, HashMap<Entity, DirtyState>>,
}

impl<DT: DataType> RequesterAPI<DT> for RequesterBookkeeping<DT> {
	fn pass_data(
		&mut self,
		requester_params: &mut DynRequesterParams<DT>,
		params: &DT::Params,
		dirty_state: DirtyState,
		data: &DT::Data,
	) {
		let Some(waiting) = self.waiting.get_mut(params) else { return; };
		let mut consuming_nodes = requester_params.get::<RequesterSystemParams<DT>>();
		for (_, entities) in btree_drain::drain_range(&mut waiting.1, ..=dirty_state) {
			for entity in entities {
				waiting.0.get_mut(&entity).expect("It was in waiting.1 and so should be in waiting.0").remove(&dirty_state);
				let Ok(mut node) = consuming_nodes.get_mut(entity) else { continue; };
				self.usages.entry(params.clone()).or_default().insert(entity, dirty_state);
				node.pass_data(params, dirty_state, data);
			}
		}
	}

	fn dirty_data(
		&mut self,
		requester_params: &mut DynRequesterParams<DT>,
		params: &DT::Params,
		last_clean_tick: u64,
		diff: Option<&DT::Diff>,
	) {
		let Some(usages) = self.usages.get_mut(params) else { return; };
		let mut consuming_nodes = requester_params.get::<RequesterSystemParams<DT>>();
		for (&usage, dirty_state) in usages {
			if matches!(dirty_state, DirtyState::UpToDate) { *dirty_state = DirtyState::Dirty { tick: last_clean_tick }; }
			let Ok(mut node) = consuming_nodes.get_mut(usage) else { continue; };
			node.dirty_data(params, last_clean_tick, diff);
		}
	}
}

#[derive(SystemParam)]
pub struct Requester<'w, 's, DT: DataType> {
	requester: ResMut<'w, RequesterBookkeeping<DT>>,
	storage: Single<'w, 's, One<&'static mut dyn StorageRequesterAPI<DT>>, With<IsResource>>,
	storage_params: DynStorageRequesterParams<'w, 's, DT>,
}

impl<DT: DataType> Requester<'_, '_, DT> {
	pub fn request(&mut self, requester_node: Entity, request: &Request<DT>) -> Option<(DT::Data, DirtyState)> {
		if let Some(data) = self.storage.request(&mut self.storage_params, request) {
			self.requester.usages.entry(request.params.clone()).or_default().insert(requester_node, request.allowed_tick);
			return Some(data);
		}
		let waiting = self.requester.waiting.entry(request.params.clone()).or_default();
		waiting.0.entry(requester_node).or_default().insert(request.allowed_tick);
		waiting.1.entry(request.allowed_tick).or_default().insert(requester_node);
		None
	}
	pub fn cancel(&mut self, requester_node: Entity, params: &DT::Params, latest: DirtyState, oldest: Option<u64>) {
		let used = self.requester.usages.get_mut(params).map_or_default(|usages| 'ret: {
			let Some(&dirty_state) = usages.get(&requester_node) else { break 'ret 0; };
			if dirty_state > latest || matches!((dirty_state, oldest), (DirtyState::Dirty { tick }, Some(oldest)) if tick < oldest) { break 'ret 0; }
			if usages.len() == 1 {
				break 'ret -1;
			}
			usages.remove(&requester_node);
			1
		});
		if used == -1 { self.requester.usages.remove(params); }
		let waited = self.requester.waiting.get_mut(params).map_or_default(|waiting| 'ret: {
			let Some(mut entity_waiting) = waiting.0.get_mut(&requester_node) else { break 'ret 0; };
			let to_remove = if let Some(oldest) = oldest {
				btree_drain::set_drain_range(&mut entity_waiting, DirtyState::Dirty { tick: oldest }..=latest)
			} else {
				entity_waiting.split_off(&latest)
			};
			if entity_waiting.is_empty() {
				if waiting.0.len() == 1 {

					break 'ret -1;
				}
				waiting.0.remove(&requester_node);
			}
			for dirty_state in to_remove {
				waiting.1.get_mut(&dirty_state).expect("It was in waiting.0 and so should be in waiting.1").remove(&requester_node);
			}
			1
		});
		if waited == -1 { self.requester.waiting.remove(params); }
		if used == 1 || waited == 1 {
			self.storage.cancel(&mut self.storage_params, params, latest, oldest);
		}
	}
}

pub trait BileRegisterSimpleConsumer {
	fn register_consumer_simple<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + ConsumingNodeAPI<DT>;
}


impl BileRegisterSimpleConsumer for App {
	fn register_consumer_simple<DT, C>(&mut self) -> &mut Self
	where DT: DataType, C: Component + ConsumingNodeAPI<DT>,
	{
		self.register_component_as::<dyn ConsumingNodeAPI<DT>, C>().register_dyn_param::<RequesterParams<DT>, RequesterSystemParams<DT>>()
	}
}
