use std::collections::HashMap;
use std::marker::PhantomData;

use bevy::prelude::*;
use bevy_trait_query::One;

use crate::dyn_system_params::DynParam;
use crate::{DataType, DirtyState, Request};
use crate::simple_api::{DynStorageFulfillerParams, DynStorageRequesterParams, FulfillerAPI, RequesterAPI, StorageFulfillerAPI, StorageFulfillerParams, StorageRequesterAPI, StorageRequesterParams};

// ============================ Simple Storage ============================

pub struct StorageRequesterSystemParams<DT: DataType>(PhantomData<DT>);

impl<DT: DataType> DynParam<StorageRequesterParams<DT>> for StorageRequesterSystemParams<DT> {
	type Param = Query<'static, 'static, One<&'static mut dyn FulfillerAPI<DT>>>;
}

pub struct StorageFulfillerSystemParams<DT: DataType>(PhantomData<DT>);

impl<DT: DataType> DynParam<StorageFulfillerParams<DT>> for StorageFulfillerSystemParams<DT> {
	type Param = Query<'static, 'static, One<&'static mut dyn RequesterAPI<DT>>>;
}

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
		storage_params: &mut DynStorageRequesterParams<DT>,
		_request: &Request<DT>,
	) -> Option<(DT::Data, DirtyState)>  {
		// check storage
		let _fulfiller_nodes = storage_params.get::<StorageRequesterSystemParams<DT>>();
		// get best cost
		// request from best
		None
	}

	fn cancel(
		&mut self,
		_storage_params: &mut DynStorageRequesterParams<DT>,
		_params: &DT::Params,
		_latest: DirtyState,
		_oldest: Option<u64>, // None if everything before latest
	) { todo!() }
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
}
