use std::marker::PhantomData;

use bevy::{
	ecs::{
		change_detection::Tick,
		system::{DynParamBuilder, DynSystemParam, DynSystemParamState, SystemAccess, SystemMeta, SystemParam, SystemParamValidationError},
		world::{DeferredWorld, unsafe_world_cell::UnsafeWorldCell},
	},
	prelude::*,
};
use bevy_trait_query::{One, TraitQuery};

pub trait HasDynSystemParam: TraitQuery {
	fn dyn_system_param(&self) -> DynParamBuilder<'static>;
}

// A `DynSystemParam` for every `T` that is in the world when the system is initialised.
pub struct DynSystemParams<'w, 's, T: ?Sized + HasDynSystemParam>(pub Vec<DynSystemParam<'w, 's>>, PhantomData<fn(&T)>);

// SAFETY: Delegates to `Vec<DynSystemParam>`, only its states are built by the `T`s instead of being empty.
unsafe impl<T: ?Sized + HasDynSystemParam> SystemParam for DynSystemParams<'_, '_, T> {
	type State = Vec<DynSystemParamState>;
	type Item<'w, 's> = DynSystemParams<'w, 's, T>;
	fn init_state(world: &mut World) -> Self::State {
		let dyn_system_params: Vec<_> = world.query::<One<&T>>().iter(world).map(|has_dyn_system_param| has_dyn_system_param.dyn_system_param()).collect();
		dyn_system_params.into_iter().map(|dyn_system_param| dyn_system_param.build(world)).collect()
	}
	fn init_access(state: &Self::State, system_meta: &mut SystemMeta, system_access: &mut SystemAccess, world: &mut World) {
		Vec::<DynSystemParam>::init_access(state, system_meta, system_access, world);
	}
	fn apply(state: &mut Self::State, system_meta: &SystemMeta, world: &mut World) { Vec::<DynSystemParam>::apply(state, system_meta, world); }
	fn queue(state: &mut Self::State, system_meta: &SystemMeta, world: DeferredWorld) { Vec::<DynSystemParam>::queue(state, system_meta, world); }
	unsafe fn get_param<'w, 's>(state: &'s mut Self::State, system_meta: &SystemMeta, world: UnsafeWorldCell<'w>, change_tick: Tick) -> Result<Self::Item<'w, 's>, SystemParamValidationError> {
		Ok(DynSystemParams(unsafe { Vec::<DynSystemParam>::get_param(state, system_meta, world, change_tick) }?, PhantomData))
	}
}

pub fn downcast<'a, T: SystemParam>(dyn_system_params: &'a mut [DynSystemParam]) -> T
where T::Item<'static, 'static>: SystemParam<Item<'a, 'a> = T> + 'static {
	dyn_system_params.iter_mut().find_map(|dyn_system_param| dyn_system_param.downcast_mut()).expect("Every API is given the param from its dyn_system_param")
}
