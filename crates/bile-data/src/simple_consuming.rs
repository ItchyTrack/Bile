use std::{collections::{HashMap, HashSet}, marker::PhantomData};

use bevy::{ecs::system::SystemParam, prelude::*};
use bevy_trait_query::One;

use crate::{DataType, DirtyState, Request, dyn_system_params::{DynParam, DynSystemParams}};

// ============================ API ============================

pub struct StorageParams<DT: DataType>(PhantomData<DT>);

impl<DT: DataType> Default for StorageParams<DT> {
	fn default() -> Self { Self(Default::default()) }
}

pub type DynStorageParams<'w, 's, DT: DataType> = DynSystemParams<'w, 's, StorageParams<DT>>;

#[bevy_trait_query::queryable]
pub trait StorageRequesterAPI<DT: DataType> {
	fn request(
		&mut self,
		storage_params: &mut DynSystemParams<StorageParams<DT>>,
		request: Request<DT>,
	) -> Option<DT::Data>;
	fn cancel(
		&mut self,
		storage_params: &mut DynSystemParams<StorageParams<DT>>,
		params: &DT::Params,
	);
}

pub struct RequesterParams<DT: DataType>(PhantomData<DT>);

impl<DT: DataType> Default for RequesterParams<DT> {
	fn default() -> Self { Self(Default::default()) }
}

pub type DynRequesterParams<'w, 's, DT: DataType> = DynSystemParams<'w, 's, RequesterParams<DT>>;

#[bevy_trait_query::queryable]
pub trait RequesterAPI<DT: DataType> {
	fn pass_data(
		&mut self,
		requester_params: &mut DynRequesterParams<DT>,
		params: &DT::Params,
		dirty_state: &DirtyState,
		data: &DT::Data,
	);
	fn dirty_data(
		&mut self,
		requester_params: &mut DynRequesterParams<DT>,
		params: &DT::Params,
		last_clean_tick: u64,
		diff: Option<&DT::Diff>,
	);
}

// ============================ Impl ============================

// Consuming API

#[bevy_trait_query::queryable]
pub trait ConsumingNodeAPI<DT: DataType> {
	fn pass_data(&mut self, params: &DT::Params, dirty_state: &DirtyState, data: &DT::Data);
	fn dirty_data(&mut self, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>);
}

// Requester

#[derive(Resource)]
pub struct RequesterBookkeeping<DT: DataType> {
	waiting: HashMap<DT::Params, HashMap<Entity, DirtyState>>,
	usages: HashMap<DT::Params, HashSet<Entity>>,
}

pub struct RequesterSystemParams<DT: DataType>(PhantomData<DT>);

impl<DT: DataType> DynParam<RequesterParams<DT>> for RequesterSystemParams<DT> {
	type Param = Query<'static, 'static, One<&'static mut dyn ConsumingNodeAPI<DT>>>;
}

impl<DT: DataType> RequesterAPI<DT> for RequesterBookkeeping<DT> {
	fn pass_data(
		&mut self,
		requester_params: &mut DynRequesterParams<DT>,
		params: &DT::Params,
		dirty_state: &DirtyState,
		data: &DT::Data,
	) {
		let Some(waiting) = self.waiting.get_mut(params) else { return; };
		let mut consuming_nodes = requester_params.get::<RequesterSystemParams<DT>>();
		waiting.extract_if(|&entity, wanted_dirty_state| {
			if !dirty_state.satisfies(wanted_dirty_state) { return false; }
			let Ok(mut node) = consuming_nodes.get_mut(entity) else { return true; };
			self.usages.entry(params.clone()).or_default().insert(entity);
			node.pass_data(params, dirty_state, data);
			true
		});
	}

	fn dirty_data(
		&mut self,
		requester_params: &mut DynRequesterParams<DT>,
		params: &DT::Params,
		last_clean_tick: u64,
		diff: Option<&DT::Diff>,
	) {
		let Some(usages) = self.usages.get(params) else { return; };
		let mut consuming_nodes = requester_params.get::<RequesterSystemParams<DT>>();
		for &usage in usages {
			let Ok(mut node) = consuming_nodes.get_mut(usage) else { continue; };
			node.dirty_data(params, last_clean_tick, diff);
		}
	}
}

#[derive(SystemParam)]
pub struct Requester<'w, 's, DT: DataType> {
	requester: ResMut<'w, RequesterBookkeeping<DT>>,
	storage: ResMut<'w, One<&'static mut dyn StorageRequesterAPI<DT>>>,
	storage_params: DynStorageParams<'w, 's, DT>,
}

impl<DT: DataType> Requester<'_, '_, DT> {
	pub fn request(&mut self, request: Request<DT>, requester_node: Entity) -> Option<DT::Data> {
		if let Some(data) = self.storage.request(self.storage_params, request) {
			self.requester.usages.entry(request.params).or_default().insert(requester_node);
			return data;
		}
		self.requester.waiting.entry(request.params).or_default().insert(requester_node, request.allowed_tick);
		None
	}
	pub fn cancel(&mut self, params: &DT::Params, requester_node: Entity) {
		self.storage.cancel(self.storage_params, params);
		// TODO: update bookkeeping using `waiting`, `usages`.
	}
}
