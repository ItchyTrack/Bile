use std::collections::{HashMap, HashSet};

use bevy::{ecs::system::SystemParam, prelude::*};
use bevy_trait_query::One;

use crate::{DataType, DirtyState, Request};

// ============================ API ============================

#[bevy_trait_query::queryable]
pub trait StorageRequesterAPI<DT: DataType> {
	fn request(&mut self, request: Request<DT>) -> Option<DT::Data>;
	fn cancel(&mut self, params: &DT::Params);
}

#[bevy_trait_query::queryable]
pub trait RequesterAPI<DT: DataType> {
	// TODO: Add dyn param for storage to pass the dyn params needed finish pass and dirty in the function call.
	fn pass_data(&mut self, params: &DT::Params, tick: u64, data: DT::Data);
	fn dirty_data(&mut self, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>);
}

// ============================ Impl ============================

// API

#[bevy_trait_query::queryable]
pub trait ConsumingNodeAPI<DT: DataType> {
	fn pass_data(&mut self, params: &DT::Params, tick: u64, data: DT::Data);
	fn dirty_data(&mut self, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>);
}

// Requester

pub struct RequesterBookkeeping<DT: DataType> {
	waiting: HashMap<DT::Params, HashMap<Entity, DirtyState>>,
	finished: HashMap<DT::Params, HashSet<Entity>>,
	usages: HashMap<DT::Params, HashSet<Entity>>,
}

impl<DT: DataType> RequesterAPI<DT> for RequesterBookkeeping<DT> {
	fn pass_data(&mut self, params: &DT::Params, tick: u64, data: DT::Data) {
		todo!()
	}

	fn dirty_data(&mut self, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>) {
		todo!()
	}
}

// impl RegisterDynParam for RequesterBookkeeping so storage can query them so pass data can go strait to Consumers

#[derive(SystemParam)]
pub struct Requester<'w, 's, DT: DataType> {
	requester: ResMut<'w, RequesterBookkeeping<DT>>,
	storage: ResMut<'w, One<&'static mut dyn StorageRequesterAPI<DT>>>,
	// system_params: DynSystemParams<'w, 's, >, // add this for storage
}

impl<DT: DataType> Requester<'_, '_, DT> {
	pub fn request(&mut self, request: Request<DT>, requester_node: Entity) -> Option<DT::Data> {
		self.storage.request(request)
		// TODO: add bookkeeping using `waiting`, `finished`, `usages`.
	}
	pub fn cancel(&mut self, params: &DT::Params, requester_node: Entity) {
		self.storage.cancel(params);
		// TODO: update bookkeeping using `waiting`, `finished`, `usages`.
	}
}
