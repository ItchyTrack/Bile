use std::{
	any::{Any, TypeId, type_name},
	marker::PhantomData,
};

use bevy::{
	ecs::{
		change_detection::Tick,
		system::{SystemAccess, SystemMeta, SystemParam, SystemParamItem, SystemParamValidationError},
		world::{DeferredWorld, unsafe_world_cell::UnsafeWorldCell},
	},
	prelude::*,
};

// A system can only use the params it was initialised with, so the params of a type that is reached dynamically
// (like the parts of a data type, through a trait object) are registered while building the app. Every system with a
// `DynSystemParams<K>` then has the params of every type registered for `K`, no matter when their entities are spawned.

/// A type that needs its own system param when it is reached through a `DynSystemParams<K>`.
/// `K` names what the param is for, so one type can need different params for different things.
pub trait DynParam<K: ?Sized + 'static>: 'static {
	type Param: SystemParam + 'static;
}

pub trait RegisterDynParam {
	/// Gives `C` its `DynParam<K>::Param` in every system with a `DynSystemParams<K>`.
	/// This has to happen before those systems are initialised, so while building the app.
	fn register_dyn_param<K: ?Sized + 'static, C: DynParam<K>>(&mut self) -> &mut Self;
}

impl RegisterDynParam for App {
	fn register_dyn_param<K: ?Sized + 'static, C: DynParam<K>>(&mut self) -> &mut Self {
		let mut registry = self.world_mut().get_resource_or_init::<DynParamRegistry<K>>();
		if !registry.inits.iter().any(|(type_id, _)| *type_id == TypeId::of::<C>()) {
			registry.inits.push((TypeId::of::<C>(), |world| Box::new(ParamState::<C::Param>(C::Param::init_state(world)))));
		}
		self
	}
}

/// The params of every type registered for `K`, each type gets its own with `get`.
pub struct DynSystemParams<'w, 's, K: ?Sized + 'static> {
	states: &'s mut [(TypeId, Box<dyn ErasedParamState>)],
	world: UnsafeWorldCell<'w>,
	system_meta: SystemMeta,
	change_tick: Tick,
	_marker: PhantomData<fn(&K)>,
}

impl<K: ?Sized + 'static> DynSystemParams<'_, '_, K> {
	pub fn get<C: DynParam<K>>(&mut self) -> SystemParamItem<'_, '_, C::Param> {
		let state = self
			.states
			.iter_mut()
			.find(|(type_id, _)| *type_id == TypeId::of::<C>())
			.and_then(|(_, state)| (state.as_mut() as &mut dyn Any).downcast_mut::<ParamState<C::Param>>())
			.unwrap_or_else(|| {
				panic!("{} has no params for {} in this system. It has to be registered before the system is initialised.", type_name::<C>(), type_name::<K>())
			});
		// SAFETY: The system registered the access of every state in `init_access` and this is the world it was
		// initialised with. `get` borrows `self` mutably, so the returned param is the only one using its access.
		unsafe { C::Param::get_param(&mut state.0, &self.system_meta, self.world, self.change_tick) }
			.unwrap_or_else(|error| panic!("The params of {} for {} are invalid: {error}", type_name::<C>(), type_name::<K>()))
	}
}

// SAFETY: Delegates to the params of every registered type, `init_access` registers the access of all of them.
unsafe impl<K: ?Sized + 'static> SystemParam for DynSystemParams<'_, '_, K> {
	type State = DynSystemParamsState;
	type Item<'w, 's> = DynSystemParams<'w, 's, K>;

	fn init_state(world: &mut World) -> Self::State {
		let inits = world.get_resource::<DynParamRegistry<K>>().map(|registry| registry.inits.clone()).unwrap_or_default();
		DynSystemParamsState(inits.into_iter().map(|(type_id, init)| (type_id, init(world))).collect())
	}
	fn init_access(state: &Self::State, system_meta: &mut SystemMeta, system_access: &mut SystemAccess, world: &mut World) {
		for (_, state) in &state.0 {
			state.init_access(system_meta, system_access, world);
		}
	}
	fn apply(state: &mut Self::State, system_meta: &SystemMeta, world: &mut World) {
		for (_, state) in &mut state.0 {
			state.apply(system_meta, world);
		}
	}
	fn queue(state: &mut Self::State, system_meta: &SystemMeta, mut world: DeferredWorld) {
		for (_, state) in &mut state.0 {
			state.queue(system_meta, world.reborrow());
		}
	}
	unsafe fn get_param<'w, 's>(
		state: &'s mut Self::State,
		system_meta: &SystemMeta,
		world: UnsafeWorldCell<'w>,
		change_tick: Tick,
	) -> Result<Self::Item<'w, 's>, SystemParamValidationError> {
		// Validated eagerly, like `DynSystemParam`, so the system is skipped when a param is unavailable.
		for (_, state) in &mut state.0 {
			// SAFETY: Upheld by the caller.
			unsafe { state.validate(system_meta, world, change_tick) }?;
		}
		Ok(DynSystemParams { states: &mut state.0, world, system_meta: system_meta.clone(), change_tick, _marker: PhantomData })
	}
}

/// The state of every registered param, built when the system is initialised.
pub struct DynSystemParamsState(Vec<(TypeId, Box<dyn ErasedParamState>)>);

#[derive(Resource)]
struct DynParamRegistry<K: ?Sized + 'static> {
	inits: Vec<(TypeId, fn(&mut World) -> Box<dyn ErasedParamState>)>,
	_marker: PhantomData<fn(&K)>,
}

impl<K: ?Sized + 'static> Default for DynParamRegistry<K> {
	fn default() -> Self { Self { inits: Vec::new(), _marker: PhantomData } }
}

// The state of a param whose type is only known to the type that registered it.
trait ErasedParamState: Any + Send + Sync {
	fn init_access(&self, system_meta: &mut SystemMeta, system_access: &mut SystemAccess, world: &mut World);
	fn apply(&mut self, system_meta: &SystemMeta, world: &mut World);
	fn queue(&mut self, system_meta: &SystemMeta, world: DeferredWorld);
	/// # Safety
	/// The same as `SystemParam::get_param`.
	unsafe fn validate(&mut self, system_meta: &SystemMeta, world: UnsafeWorldCell, change_tick: Tick) -> Result<(), SystemParamValidationError>;
}

struct ParamState<P: SystemParam>(P::State);

impl<P: SystemParam + 'static> ErasedParamState for ParamState<P> {
	fn init_access(&self, system_meta: &mut SystemMeta, system_access: &mut SystemAccess, world: &mut World) {
		P::init_access(&self.0, system_meta, system_access, world);
	}
	fn apply(&mut self, system_meta: &SystemMeta, world: &mut World) { P::apply(&mut self.0, system_meta, world); }
	fn queue(&mut self, system_meta: &SystemMeta, world: DeferredWorld) { P::queue(&mut self.0, system_meta, world); }
	unsafe fn validate(&mut self, system_meta: &SystemMeta, world: UnsafeWorldCell, change_tick: Tick) -> Result<(), SystemParamValidationError> {
		// SAFETY: Upheld by the caller.
		unsafe { P::get_param(&mut self.0, system_meta, world, change_tick) }.map(drop)
	}
}
