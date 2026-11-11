use std::collections::HashMap;

use bevy::{ecs::component::{ComponentId, ComponentIdFor}, prelude::*, transform::TransformSystems};
use bevy_trait_query::RegisterExt;
use bile_data::{BileAddDataType, DataConsumingNode, DataRequester, DataType, DirtyState, Request};

#[cfg(test)]
mod tests;

pub trait ComponentData: DataType<Data = Option<Self::Component>> {
	type Component: Component + Clone;
}

#[derive(Debug, Clone, Copy)]
struct WantState {
	count: u32,
	want: ComponentId,
	resolved: bool,
}

#[derive(Component, Debug, Default)]
pub struct Wants {
	wanted: HashMap<ComponentId, WantState>,
}

impl Wants {
	pub fn is_wanted(&self, component: ComponentId) -> bool {
		self.wanted.contains_key(&component)
	}
	pub fn is_resolved(&self, component: ComponentId) -> bool {
		self.wanted.get(&component).is_some_and(|state| state.resolved)
	}
}

#[derive(Component)]
pub struct Want<DT: ComponentData> {
	params: DT::Params,
	requested: bool,
	received: Option<DT::Data>,
}

impl<DT: ComponentData> Want<DT> {
	pub fn params(&self) -> &DT::Params { &self.params }
}

impl<DT: ComponentData> DataConsumingNode<DT> for Want<DT> {
	fn pass_data(&mut self, _params: &DT::Params, _tick: u64, data: DT::Data) {
		self.received = Some(data);
	}
	fn dirty_data(&mut self, _params: &DT::Params, _last_clean_tick: u64) {
		self.requested = false;
	}
}

pub fn want<DT: ComponentData>(entity: &mut EntityWorldMut, params: DT::Params) {
	let (component, want) = entity.world_scope(|world| (world.register_component::<DT::Component>(), world.register_component::<Want<DT>>()));
	if !entity.contains::<Wants>() {
		entity.insert(Wants::default());
	}
	let mut wants = entity.get_mut::<Wants>().unwrap();
	if let Some(state) = wants.wanted.get_mut(&component) {
		assert!(state.want == want, "an entity can only want a component from one data type");
		state.count += 1;
		assert!(entity.get::<Want<DT>>().is_some_and(|existing| existing.params == params), "an entity can only want a component with one set of params");
		return;
	}
	wants.wanted.insert(component, WantState { count: 1, want, resolved: false });
	entity.insert(Want::<DT> { params, requested: false, received: None });
}

pub fn unwant(entity: &mut EntityWorldMut, component: ComponentId) {
	let Some(mut wants) = entity.get_mut::<Wants>() else { return };
	let Some(state) = wants.wanted.get_mut(&component) else {
		bevy::log::warn!("Negative want on component. This should not happen with correct usage.");
		return;
	};
	state.count -= 1;
	if state.count > 0 { return; }
	let want = state.want;
	wants.wanted.remove(&component);
	entity.remove_by_ids(&[want, component]);
}

pub trait WantCommands {
	fn want<DT: ComponentData>(&mut self, params: DT::Params) -> &mut Self;
	fn unwant<DT: ComponentData>(&mut self) -> &mut Self;
	fn unwant_by_id(&mut self, component: ComponentId) -> &mut Self;
}

impl WantCommands for EntityCommands<'_> {
	fn want<DT: ComponentData>(&mut self, params: DT::Params) -> &mut Self {
		self.queue(move |mut entity: EntityWorldMut| want::<DT>(&mut entity, params))
	}
	fn unwant<DT: ComponentData>(&mut self) -> &mut Self {
		self.queue_silenced(|mut entity: EntityWorldMut| {
			let component = entity.world_scope(|world| world.register_component::<DT::Component>());
			unwant(&mut entity, component);
		})
	}
	fn unwant_by_id(&mut self, component: ComponentId) -> &mut Self {
		self.queue_silenced(move |mut entity: EntityWorldMut| unwant(&mut entity, component))
	}
}

#[derive(Resource)]
struct RemovedWants<DT: ComponentData> {
	removed: Vec<(Entity, DT::Params)>,
}

impl<DT: ComponentData> Default for RemovedWants<DT> {
	fn default() -> Self {
		Self { removed: Vec::new() }
	}
}

fn queue_removed_want<DT: ComponentData>(remove: On<Remove, Want<DT>>, wants: Query<&Want<DT>>, mut removed: ResMut<RemovedWants<DT>>) {
	let Ok(want) = wants.get(remove.entity) else { return };
	removed.removed.push((remove.entity, want.params.clone()));
}

fn update_wants<DT: ComponentData>(
	mut commands: Commands,
	component: ComponentIdFor<DT::Component>,
	mut requester: DataRequester<DT>,
	mut removed: ResMut<RemovedWants<DT>>,
	mut wants: Query<(Entity, &mut Want<DT>, &mut Wants), Changed<Want<DT>>>,
) {
	for (entity, params) in removed.removed.drain(..) {
		requester.cancel(&params, entity);
	}
	for (entity, mut want, mut wants) in &mut wants {
		let want = want.bypass_change_detection();
		if !want.requested {
			want.requested = true;
			if let Some(data) = requester.request(Request::new(want.params.clone(), DirtyState::UpToDate), entity) {
				want.received = Some(data);
			}
		}
		let Some(data) = want.received.take() else { continue };
		match data {
			Some(data) => { commands.entity(entity).try_insert(data); }
			None => { commands.entity(entity).try_remove::<DT::Component>(); }
		}
		if let Some(state) = wants.wanted.get_mut(&component.get()) {
			state.resolved = true;
		}
	}
}

pub trait AddComponentData {
	fn add_component_data<DT: ComponentData>(&mut self) -> &mut Self;
}

impl AddComponentData for App {
	fn add_component_data<DT: ComponentData>(&mut self) -> &mut Self {
		self.bile_add_data_type::<DT>();
		self.init_resource::<RemovedWants<DT>>()
			.register_component_as::<dyn DataConsumingNode<DT>, Want<DT>>()
			.add_observer(queue_removed_want::<DT>)
			.add_systems(PostUpdate, update_wants::<DT>.before(TransformSystems::Propagate))
	}
}
