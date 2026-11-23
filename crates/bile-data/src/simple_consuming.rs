use std::{collections::{BTreeMap, BTreeSet, HashMap, HashSet}, marker::PhantomData, ops::RangeBounds};

use bevy::{ecs::{resource::IsResource, system::SystemParam}, prelude::*};
use bevy_trait_query::{One, RegisterExt};

use crate::{btree_drain, cancel_range, unheld_ranges, Claim, DataType, DirtyState, Request, dyn_system_params::{DynParam, RegisterDynParam}};
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
	claims: HashMap<DT::Params, Claim>, // What it holds at the storage
}

impl<DT: DataType> RequesterBookkeeping<DT> {
	// The states some node still waits for or uses
	fn held(&self, params: &DT::Params) -> Vec<DirtyState> {
		let waiting = self.waiting.get(params).into_iter().flat_map(|waiting| waiting.1.keys().copied());
		waiting.chain(self.usages.get(params).into_iter().flat_map(|usages| usages.values().copied())).collect()
	}
	fn remove_waits(&mut self, params: &DT::Params, node: Entity, range: impl RangeBounds<DirtyState>) -> BTreeSet<DirtyState> {
		let Some(waiting) = self.waiting.get_mut(params) else { return BTreeSet::new(); };
		let Some(node_waiting) = waiting.0.get_mut(&node) else { return BTreeSet::new(); };
		let removed = btree_drain::set_drain_range(node_waiting, range);
		if node_waiting.is_empty() { waiting.0.remove(&node); }
		for dirty_state in &removed {
			let entities = waiting.1.get_mut(dirty_state).expect("It was in waiting.0 and so should be in waiting.1");
			entities.remove(&node);
			if entities.is_empty() { waiting.1.remove(dirty_state); }
		}
		if waiting.0.is_empty() { self.waiting.remove(params); }
		removed
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
		if let Some(claim) = self.claims.get_mut(params) { claim.pass(dirty_state); }
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
		let served = self.storage.request(&mut self.storage_params, self.requester.entity, request);
		self.requester.claims.entry(request.params.clone()).or_default().request(request.allowed_tick, served.is_some());
		if let Some((data, dirty_state)) = served {
			// The storage may still pass the data for the waits it satisfies, the node has it already
			let satisfied = self.requester.remove_waits(&request.params, requester_node, ..=dirty_state);
			let wanted = satisfied.last().map_or(request.allowed_tick, |&wanted| wanted.max(request.allowed_tick));
			self.requester.usages.entry(request.params.clone()).or_default().insert(requester_node, wanted);
			return Some((data, dirty_state));
		}
		let waiting = self.requester.waiting.entry(request.params.clone()).or_default();
		waiting.0.entry(requester_node).or_default().insert(request.allowed_tick);
		waiting.1.entry(request.allowed_tick).or_default().insert(requester_node);
		None
	}
	pub fn cancel(&mut self, requester_node: Entity, params: &DT::Params, latest: DirtyState, oldest: Option<u64>) {
		let requester = &mut *self.requester;
		if let Some(usages) = requester.usages.get_mut(params) {
			if usages.get(&requester_node).is_some_and(|wanted| wanted.is_cancelled_by(latest, oldest)) { usages.remove(&requester_node); }
			if usages.is_empty() { requester.usages.remove(params); }
		}
		requester.remove_waits(params, requester_node, cancel_range(latest, oldest));
		let held = requester.held(params);
		let Some(claim) = requester.claims.get_mut(params) else { return; };
		if held.is_empty() {
			requester.claims.remove(params);
			self.storage.cancel(&mut self.storage_params, requester.entity, params, DirtyState::UpToDate, None);
			return;
		}
		// Only what no other node still waits for or uses, and not what it uses at the storage
		for (latest, oldest) in unheld_ranges(held.into_iter().chain(claim.usage), latest, oldest) {
			if claim.cancel(latest, oldest) { self.storage.cancel(&mut self.storage_params, requester.entity, params, latest, oldest); }
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
			self.insert_resource(RequesterBookkeeping::<DT> { entity: Entity::PLACEHOLDER, waiting: HashMap::default(), usages: HashMap::default(), claims: HashMap::default() });
			let entity = self.world().resource_entity::<RequesterBookkeeping<DT>>().expect("It was just inserted");
			self.world_mut().resource_mut::<RequesterBookkeeping<DT>>().entity = entity;
		}
		self.register_component_as::<dyn ConsumingNodeAPI<DT>, C>()
			.register_component_as::<dyn RequesterAPI<DT>, RequesterBookkeeping<DT>>()
			.register_dyn_param::<RequesterParams<DT>, RequesterSystemParams<DT>>()
	}
}
