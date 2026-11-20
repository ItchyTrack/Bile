use bevy::{ecs::system::SystemParam, prelude::*};
use bevy_trait_query::One;

use crate::{DataType, Request, dyn_system_params::DynSystemParams};

// ============================ API ============================

pub trait StorageConsumingAPI<DT: DataType> {
	fn request(&mut self, request: Request<DT>);
	fn cancel(&mut self, params: &DT::Params);
}

pub trait RequesterAPI<DT: DataType> {
	fn pass_data(&mut self, params: &DT::Params, tick: u64, data: DT::Data);
	fn dirty_data(&mut self, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>);
}

// ============================ Consuming Impl ============================

// API

#[bevy_trait_query::queryable]
pub trait ConsumingNodeAPI<DT: DataType> {
	fn pass_data(&mut self, params: &DT::Params, tick: u64, data: DT::Data);
	fn dirty_data(&mut self, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>);
}

pub struct Requester<DT: DataType> {
	waiting: HashMap<DT::Params, HashMap<Entity, DirtyState>>,
	finished: HashMap<DT::Params, HashSet<Entity>>,
	usages: HashMap<DT::Params, HashSet<Entity>>,
}

impl<DT: DataType> RequesterAPI<DT> for Requester<DT> {
	fn pass_data(&mut self, params: &DT::Params, tick: u64, data: DT::Data) {
		todo!()
	}

	fn dirty_data(&mut self, params: &DT::Params, last_clean_tick: u64, diff: Option<&DT::Diff>) {
		todo!()
	}
}

#[derive(SystemParam)]
pub struct DataRequester<'w, 's, DT: DataType> {
	requester: Query<'w, 's, One<&'static mut dyn NodeRequester<DT>>>,
	system_params: DynSystemParams<'w, 's, Requesting<DT>>,
}

impl<DT: DataType> DataRequester<'_, '_, DT> {
	pub fn request(&mut self, request: Request<DT>, requester_node: Entity) -> Option<DT::Data> {
		let Ok((mut requester, mut storage)) = self.requester.single_mut() else {
			panic!("{} has no requester for nodes, a consuming node of it has to be registered.", type_name::<DT>())
		};
		requester.request(&mut *storage, &mut self.system_params, &mut self.fulfillers, request, requester_node)
	}
	pub fn cancel(&mut self, params: &DT::Params, requester_node: Entity) {
		if let Ok((mut requester, mut storage)) = self.requester.single_mut() && requester.cancel(&mut *storage, params, requester_node) {
			self.fulfillers.release(&mut *storage, params);
		}
	}
}
