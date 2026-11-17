use std::hash::Hash;

use bevy::{
	app::MainScheduleOrder,
	ecs::{
		change_detection::Tick,
		schedule::ScheduleLabel,
		system::{DynParamBuilder, DynSystemParam, DynSystemParamState, SystemAccess, SystemMeta, SystemParam, SystemParamValidationError},
		world::{DeferredWorld, unsafe_world_cell::UnsafeWorldCell},
	},
	prelude::*,
};
use bevy_trait_query::One;

#[derive(Resource, Default)]
pub struct StateTick {
	current_tick: u64,
}

impl StateTick {
	pub fn current_tick(&self) -> u64 {
		self.current_tick
	}
}

pub trait DataType: 'static {
	type Params: Clone + Hash + PartialEq + Eq + Send + Sync + 'static;
	type Data: Clone + Send + Sync + 'static;
	type Diff: Clone + Send + Sync + 'static;
}

// A set of `DataType` data that is keyed by its own params.
pub trait StreamingDataType: 'static {
	type DataType: DataType;
	type Params: Clone + Hash + PartialEq + Eq + Send + Sync + 'static;
}

#[derive(Clone, Copy)]
pub enum DirtyState {
	UpToDate,
	Dirty{tick: u64}
}

impl DirtyState {
	pub fn satisfies(&self, required: &DirtyState) -> bool {
		match (self, required) {
			(DirtyState::UpToDate, _) => true,
			(_, DirtyState::UpToDate) => false,
			(DirtyState::Dirty { tick: have }, DirtyState::Dirty { tick: need }) => *have >= *need,
		}
	}

	pub fn is_fresher_than(&self, other: &DirtyState) -> bool {
		match (self, other) {
			(DirtyState::UpToDate, DirtyState::UpToDate) => false,
			(DirtyState::UpToDate, _) => true,
			(_, DirtyState::UpToDate) => false,
			(DirtyState::Dirty { tick: a }, DirtyState::Dirty { tick: b }) => a > b,
		}
	}
}

pub struct Request<DT: DataType> {
	pub params: DT::Params,
	pub allowed_tick: DirtyState, // UpToDate -> Only up to date data; Dirty(tick) -> Reject data before tick
}

impl<DT: DataType> Request<DT> {
	pub fn new(params: DT::Params, allowed_tick: DirtyState) -> Self {
		Self { params, allowed_tick }
	}
}

pub trait ApiRequest: 'static {
	type Params;
	type Response;
	fn params(&self) -> &Self::Params;
}

impl<DT: DataType> ApiRequest for Request<DT> {
	type Params = DT::Params;
	type Response = Option<DT::Data>;
	fn params(&self) -> &DT::Params { &self.params }
}

#[bevy_trait_query::queryable]
pub trait DataRequestingAPI<R: ApiRequest> {
	fn dyn_system_param(&self) -> DynParamBuilder<'static>;
	fn cost(&self, dyn_system_params: &mut [DynSystemParam], request: &R) -> Option<f32>;
	fn request(&mut self, dyn_system_params: &mut [DynSystemParam], request: R, requester_node: Entity) -> R::Response;
	fn cancel(&mut self, dyn_system_params: &mut [DynSystemParam], params: &R::Params, requester_node: Entity);
}

pub struct DataRequester<'w, 's, R: ApiRequest> {
	data_requesting_apis: Query<'w, 's, (One<&'static mut dyn DataRequestingAPI<R>>, Entity)>,
	dyn_system_params: Vec<DynSystemParam<'w, 's>>,
}

// SAFETY: Delegates to the tuple of its fields, only the states of the `DynSystemParam`s are built by the APIs instead of being empty.
unsafe impl<R: ApiRequest> SystemParam for DataRequester<'_, '_, R> {
	type State = (QueryState<(One<&'static mut dyn DataRequestingAPI<R>>, Entity)>, Vec<DynSystemParamState>);
	type Item<'w, 's> = DataRequester<'w, 's, R>;
	fn init_state(world: &mut World) -> Self::State {
		let dyn_system_params: Vec<_> = world.query::<One<&dyn DataRequestingAPI<R>>>().iter(world).map(|data_requesting_api| data_requesting_api.dyn_system_param()).collect();
		(Query::init_state(world), dyn_system_params.into_iter().map(|dyn_system_param| dyn_system_param.build(world)).collect())
	}
	fn init_access(state: &Self::State, system_meta: &mut SystemMeta, system_access: &mut SystemAccess, world: &mut World) {
		<(Query<_>, Vec<DynSystemParam>)>::init_access(state, system_meta, system_access, world);
	}
	fn apply(state: &mut Self::State, system_meta: &SystemMeta, world: &mut World) { <(Query<_>, Vec<DynSystemParam>)>::apply(state, system_meta, world); }
	fn queue(state: &mut Self::State, system_meta: &SystemMeta, world: DeferredWorld) { <(Query<_>, Vec<DynSystemParam>)>::queue(state, system_meta, world); }
	unsafe fn get_param<'w, 's>(state: &'s mut Self::State, system_meta: &SystemMeta, world: UnsafeWorldCell<'w>, change_tick: Tick) -> Result<Self::Item<'w, 's>, SystemParamValidationError> {
		let (data_requesting_apis, dyn_system_params) = unsafe { <(Query<_>, Vec<DynSystemParam>)>::get_param(state, system_meta, world, change_tick) }?;
		Ok(DataRequester { data_requesting_apis, dyn_system_params })
	}
}

impl<'w, 's, R: ApiRequest> DataRequester<'w, 's, R> {
	pub fn request(&mut self, request: R, requester_node: Entity) -> R::Response {
		let api_entity = best(self.data_requesting_apis.iter().map(|(data_requesting_api, entity)| (data_requesting_api.cost(&mut self.dyn_system_params, &request), entity)))
			.expect("Data request failed to find node to fulfill the request. This should never happen!")
			.1;
		for (mut data_requesting_api, entity) in self.data_requesting_apis.iter_mut() {
			if entity == api_entity { continue; }
			data_requesting_api.cancel(&mut self.dyn_system_params, request.params(), requester_node);
		}
		self.data_requesting_apis.get_mut(api_entity).expect("This must exist as it was found in best_node").0.request(&mut self.dyn_system_params, request, requester_node)
	}
	pub fn cancel(&mut self, params: &R::Params, requester_node: Entity) {
		for (mut data_requesting_api, _entity) in self.data_requesting_apis.iter_mut() {
			data_requesting_api.cancel(&mut self.dyn_system_params, params, requester_node);
		}
	}
}

pub fn downcast<'a, T: SystemParam>(dyn_system_params: &'a mut [DynSystemParam]) -> T
where T::Item<'static, 'static>: SystemParam<Item<'a, 'a> = T> + 'static {
	dyn_system_params.iter_mut().find_map(|dyn_system_param| dyn_system_param.downcast_mut()).expect("Every API is given the param from its dyn_system_param")
}

pub(crate) fn best(costs: impl Iterator<Item = (Option<f32>, Entity)>) -> Option<(f32, Entity)> {
	costs.filter_map(|(cost, entity)| Some((cost?, entity))).min_by(|a, b| a.0.total_cmp(&b.0))
}

fn update_bile_state_tick(mut bile_state_tick: ResMut<StateTick>) {
	bile_state_tick.current_tick += 1;
}

#[derive(ScheduleLabel, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BileDataSchedule;

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BileDataSystemSet {
	DropData,
	PassData,
	UpdateStateTick,
}

pub struct BileDataDefaultPlugin;

impl Plugin for BileDataDefaultPlugin {
	fn build(&self, app: &mut App) {
		app.init_schedule(BileDataSchedule);
		app.world_mut()
			.resource_mut::<MainScheduleOrder>()
			.insert_after(Update, BileDataSchedule);

		app.configure_sets(
			BileDataSchedule,
			(
				BileDataSystemSet::DropData,
				BileDataSystemSet::PassData,
				BileDataSystemSet::UpdateStateTick,
			).chain(),
		);

		app.init_resource::<StateTick>();
		app.add_systems(BileDataSchedule, update_bile_state_tick.in_set(BileDataSystemSet::UpdateStateTick));
	}
}
