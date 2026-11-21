use std::collections::HashMap;

use bevy::prelude::*;

use crate::{DataType, DirtyState, Request};
use crate::simple_api::{DynStorageFulfillerParams, DynStorageRequesterParams, StorageFulfillerAPI, StorageRequesterAPI};

// ============================ Simple Storage ============================

/// Keeps the data in a column.
#[derive(Component)]
pub struct Storage<DT: DataType> {
	data: HashMap<DT::Params, (DirtyState, Option<DT::Data>)>,
}

impl<DT: DataType> Default for Storage<DT> {
	fn default() -> Self { Self { data: HashMap::default() } }
}

impl<DT: DataType> Storage<DT> {
	pub fn get(&self, params: &DT::Params) -> Option<(DirtyState, DT::Data)> {
		let (dirty_state, data) = self.data.get(params)?;
		Some((*dirty_state, data.clone()?))
	}
}

impl<DT: DataType> StorageRequesterAPI<DT> for Storage<DT> {
	fn request(
		&mut self,
		_storage_params: &mut DynStorageRequesterParams<DT>,
		_request: &Request<DT>,
	) -> Option<(DT::Data, DirtyState)>  { todo!() }

	fn cancel(
		&mut self,
		_storage_params: &mut DynStorageRequesterParams<DT>,
		_params: &DT::Params,
		_latest: DirtyState,
		_oldest: Option<u64>, // None if everything before latest
	) { todo!() }

	// type Param = ();

	// fn data(&self, _param: &(), params: &DT::Params) -> Option<(DirtyState, DT::Data)> { self.get(params) }
	// fn requests(&mut self) -> &mut Requests<DT> { &mut self.requests }
	// fn release(&mut self, params: &DT::Params) -> bool { self.data.remove(params).is_some() }
}

impl<DT: DataType> StorageFulfillerAPI<DT> for Storage<DT> {
	fn pass_data(
		&mut self,
		_storage_params: &mut DynStorageFulfillerParams<DT>,
		_params: &DT::Params,
		_dirty_state: DirtyState,
		_data: DT::Data,
	) { todo!() }

	fn dirty_data(
		&mut self,
		_storage_params: &mut DynStorageFulfillerParams<DT>,
		_params: &DT::Params,
		_last_clean_tick: u64,
		_diff: Option<&DT::Diff>,
	) { todo!() }

	// type Param = ();

	// fn data(&self, _param: &(), params: &DT::Params) -> Option<(DirtyState, DT::Data)> { self.get(params) }
	// fn dirty_state(&self, params: &DT::Params) -> Option<DirtyState> { self.data.get(params).map(|(dirty_state, _)| *dirty_state) }
	// fn wanted(&self, params: &DT::Params) -> bool { self.requests.is_waiting(params) }
	// fn is_requested(&self, params: &DT::Params, allowed_tick: DirtyState) -> bool { self.requests.is_waiting_for(params, allowed_tick) }
	// fn store(&mut self, params: &DT::Params, dirty_state: DirtyState, data: Option<DT::Data>) {
	// 	if data.is_some() {
	// 		self.requests.stored(params, dirty_state);
	// 	}
	// 	self.data.insert(params.clone(), (dirty_state, data));
	// }
	// fn mark_dirty(&mut self, params: &DT::Params, last_clean_tick: u64) -> bool {
	// 	let Some((dirty_state, _)) = self.data.get_mut(params) else { return false; };
	// 	*dirty_state = DirtyState::Dirty { tick: last_clean_tick };
	// 	true
	// }
}
