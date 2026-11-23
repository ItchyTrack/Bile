use std::{collections::{BTreeMap, BTreeSet, HashMap, HashSet}, marker::PhantomData};

use bevy::{ecs::{resource::IsResource, system::SystemParam}, prelude::*};
use bevy_trait_query::{One, RegisterExt};

use crate::{btree_drain, cancel_range, unheld_ranges, DataType, DirtyState, Request, dyn_system_params::{DynParam, RegisterDynParam}};
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
	entity: Entity, // The resource entity, the storage knows the requester by it
	waiting: HashMap<DT::Params, (HashMap<Entity, BTreeSet<DirtyState>>, BTreeMap<DirtyState, HashSet<Entity>>)>,
	usages: HashMap<DT::Params, HashMap<Entity, DirtyState>>, // Params -> Consuming node -> Wanted dirty state
}

impl<DT: DataType> RequesterBookkeeping<DT> {
	// The states some node still waits for or uses
	fn held(&self, params: &DT::Params) -> Vec<DirtyState> {
		let waiting = self.waiting.get(params).into_iter().flat_map(|waiting| waiting.1.keys().copied());
		waiting.chain(self.usages.get(params).into_iter().flat_map(|usages| usages.values().copied())).collect()
	}
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
		// A node gets the data once, for the freshest of its requests it satisfies
		let mut satisfied = HashMap::<Entity, DirtyState>::new();
		for (wanted_dirty_state, entities) in btree_drain::drain_range(&mut waiting.1, ..=dirty_state) {
			for entity in entities {
				let entity_waiting = waiting.0.get_mut(&entity).expect("It was in waiting.1 and so should be in waiting.0");
				entity_waiting.remove(&wanted_dirty_state);
				if entity_waiting.is_empty() { waiting.0.remove(&entity); }
				satisfied.insert(entity, wanted_dirty_state);
			}
		}
		if waiting.0.is_empty() { self.waiting.remove(params); }
		for (entity, wanted_dirty_state) in satisfied {
			let Ok(mut node) = consuming_nodes.get_mut(entity) else { continue; };
			self.usages.entry(params.clone()).or_default().insert(entity, wanted_dirty_state);
			node.pass_data(params, dirty_state, data);
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
		for (&usage, _wanted_dirty_state) in usages {
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
		if let Some(data) = self.storage.request(&mut self.storage_params, self.requester.entity, request) {
			self.requester.usages.entry(request.params.clone()).or_default().insert(requester_node, request.allowed_tick);
			return Some(data);
		}
		let waiting = self.requester.waiting.entry(request.params.clone()).or_default();
		waiting.0.entry(requester_node).or_default().insert(request.allowed_tick);
		waiting.1.entry(request.allowed_tick).or_default().insert(requester_node);
		None
	}
	pub fn cancel(&mut self, requester_node: Entity, params: &DT::Params, latest: DirtyState, oldest: Option<u64>) {
		let used = self.requester.usages.get_mut(params).is_some_and(|usages| 'ret: {
			let Some(&wanted_dirty_state) = usages.get(&requester_node) else { break 'ret false; };
			if !wanted_dirty_state.is_cancelled_by(latest, oldest) { break 'ret false; }
			usages.remove(&requester_node);
			true
		});
		if self.requester.usages.get(params).is_some_and(HashMap::is_empty) { self.requester.usages.remove(params); }
		let waited = self.requester.waiting.get_mut(params).is_some_and(|waiting| 'ret: {
			let Some(entity_waiting) = waiting.0.get_mut(&requester_node) else { break 'ret false; };
			let to_remove = btree_drain::set_drain_range(entity_waiting, cancel_range(latest, oldest));
			if entity_waiting.is_empty() { waiting.0.remove(&requester_node); }
			for dirty_state in &to_remove {
				let entities = waiting.1.get_mut(dirty_state).expect("It was in waiting.0 and so should be in waiting.1");
				entities.remove(&requester_node);
				if entities.is_empty() { waiting.1.remove(dirty_state); }
			}
			!to_remove.is_empty()
		});
		if self.requester.waiting.get(params).is_some_and(|waiting| waiting.0.is_empty()) { self.requester.waiting.remove(params); }
		if used || waited {
			// Only what no other node still waits for or uses
			for (latest, oldest) in unheld_ranges(self.requester.held(params), latest, oldest) {
				self.storage.cancel(&mut self.storage_params, self.requester.entity, params, latest, oldest);
			}
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
		if !self.world().contains_resource::<RequesterBookkeeping<DT>>() {
			self.insert_resource(RequesterBookkeeping::<DT> { entity: Entity::PLACEHOLDER, waiting: HashMap::default(), usages: HashMap::default() });
			let entity = self.world().resource_entity::<RequesterBookkeeping<DT>>().expect("It was just inserted");
			self.world_mut().resource_mut::<RequesterBookkeeping<DT>>().entity = entity;
		}
		self.register_component_as::<dyn ConsumingNodeAPI<DT>, C>()
			.register_component_as::<dyn RequesterAPI<DT>, RequesterBookkeeping<DT>>()
			.register_dyn_param::<RequesterParams<DT>, RequesterSystemParams<DT>>()
	}
}
