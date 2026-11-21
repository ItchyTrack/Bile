use std::collections::HashMap;

use bevy::prelude::*;

use crate::{Request, DataType, DirtyState};

use crate::{simple_consuming, simple_producing};

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
	fn get(&self, params: &DT::Params) -> Option<(DirtyState, DT::Data)> {
		let (dirty_state, data) = self.data.get(params)?;
		Some((*dirty_state, data.clone()?))
	}
}

impl<DT: DataType> simple_consuming::StorageRequesterAPI<DT> for Storage<DT> {
	fn request(
		&mut self,
		storage_params: &mut simple_consuming::DynStorageParams<DT>,
		request: Request<DT>,
	) -> Option<DT::Data>  { todo!() }

	fn cancel(
		&mut self,
		storage_params: &mut simple_consuming::DynStorageParams<DT>,
		params: &DT::Params,
	) { todo!() }

	// type Param = ();

	// fn data(&self, _param: &(), params: &DT::Params) -> Option<(DirtyState, DT::Data)> { self.get(params) }
	// fn requests(&mut self) -> &mut Requests<DT> { &mut self.requests }
	// fn release(&mut self, params: &DT::Params) -> bool { self.data.remove(params).is_some() }
}

impl<DT: DataType> simple_producing::StorageFulfillerAPI<DT> for Storage<DT> {
	fn pass_data(
		&mut self,
		storage_params: &mut simple_producing::DynStorageParams<DT>,
		params: &DT::Params,
		tick: u64,
		data: DT::Data,
	) { todo!() }

	fn dirty_data(
		&mut self,
		storage_params: &mut simple_producing::DynStorageParams<DT>,
		params: &DT::Params,
		last_clean_tick: u64,
		diff: Option<&DT::Diff>,
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
