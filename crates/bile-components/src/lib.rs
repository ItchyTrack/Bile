use std::collections::HashMap;

use bevy::{ecs::component::{ComponentId, ComponentIdFor}, prelude::*, transform::TransformSystems};
use bile_data::{DataType, DirtyState, Request, single_cached::{self, BileRegisterSingleCached}};

#[cfg(test)]
mod tests;

pub trait ComponentData: DataType<Data = Option<Self::Component>> {
	type Component: Component + Clone;
}

#[derive(Debug, Clone, Copy, Message)]
pub struct ComponentResolved {
	pub entity: Entity,
	pub component: ComponentId,
}

#[derive(Resource, Default)]
struct ComponentDataRegistry {
	wants_by_component: HashMap<ComponentId, Vec<(ComponentId, fn(&mut EntityWorldMut))>>,
}

#[derive(Component)]
pub struct Want<DT: ComponentData> {
	params: DT::Params,
	count: u32,
	requested: bool,
	resolved: bool,
	received: Option<DT::Data>,
}

impl<DT: ComponentData> Want<DT> {
	pub fn params(&self) -> &DT::Params { &self.params }
	pub fn is_resolved(&self) -> bool { self.resolved }
}

impl<DT: ComponentData> single_cached::DataConsumingNode<DT> for Want<DT> {
	fn pass_data(&mut self, _params: &DT::Params, _tick: u64, data: DT::Data) {
		self.received = Some(data);
	}
	fn dirty_data(&mut self, _params: &DT::Params, _last_clean_tick: u64, _diff: Option<&DT::Diff>) {
		self.requested = false;
	}
}

fn wants_for(entity: &mut EntityWorldMut, component: ComponentId) -> Vec<(ComponentId, fn(&mut EntityWorldMut))> {
	entity.world_scope(|world| world.resource::<ComponentDataRegistry>().wants_by_component.get(&component).cloned().unwrap_or_default())
}

pub fn want<DT: ComponentData>(entity: &mut EntityWorldMut, params: DT::Params) {
	if let Some(mut want) = entity.get_mut::<Want<DT>>() {
		assert!(want.params == params, "an entity can only want a component with one set of params");
		want.bypass_change_detection().count += 1;
		return;
	}
	let (component, want) = entity.world_scope(|world| (world.register_component::<DT::Component>(), world.register_component::<Want<DT>>()));
	assert!(
		!wants_for(entity, component).iter().any(|&(other, _)| other != want && entity.contains_id(other)),
		"an entity can only want a component from one data type",
	);
	entity.insert(Want::<DT> { params, count: 1, requested: false, resolved: false, received: None });
}

pub fn unwant<DT: ComponentData>(entity: &mut EntityWorldMut) {
	let Some(mut want) = entity.get_mut::<Want<DT>>() else {
		bevy::log::warn!("Negative want on component. This should not happen with correct usage.");
		return;
	};
	let want = want.bypass_change_detection();
	want.count -= 1;
	if want.count == 0 {
		entity.remove::<(Want<DT>, DT::Component)>();
	}
}

pub fn unwant_by_id(entity: &mut EntityWorldMut, component: ComponentId) {
	match wants_for(entity, component).into_iter().find(|&(want, _)| entity.contains_id(want)) {
		Some((_, unwant)) => unwant(entity),
		None => bevy::log::warn!("Negative want on component. This should not happen with correct usage."),
	}
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
		self.queue_silenced(|mut entity: EntityWorldMut| unwant::<DT>(&mut entity))
	}
	fn unwant_by_id(&mut self, component: ComponentId) -> &mut Self {
		self.queue_silenced(move |mut entity: EntityWorldMut| unwant_by_id(&mut entity, component))
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

fn queue_removed_want<DT: ComponentData>(remove: On<Remove<Want<DT>>>, wants: Query<&Want<DT>>, mut removed: ResMut<RemovedWants<DT>>) {
	let Ok(want) = wants.get(remove.entity) else { return };
	removed.removed.push((remove.entity, want.params.clone()));
}

fn update_wants<DT: ComponentData>(
	mut commands: Commands,
	component: ComponentIdFor<DT::Component>,
	mut requester: single_cached::DataRequester<DT>,
	mut removed: ResMut<RemovedWants<DT>>,
	mut resolved: MessageWriter<ComponentResolved>,
	mut wants: Query<(Entity, &mut Want<DT>), Changed<Want<DT>>>,
) {
	for (entity, params) in removed.removed.drain(..) {
		requester.cancel(&params, entity);
	}
	for (entity, mut want) in &mut wants {
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
		if !want.resolved {
			want.resolved = true;
			resolved.write(ComponentResolved { entity, component: component.get() });
		}
	}
}

pub trait AddComponentData {
	fn add_component_data<DT: ComponentData>(&mut self) -> &mut Self;
}

impl AddComponentData for App {
	fn add_component_data<DT: ComponentData>(&mut self) -> &mut Self {
		let component = self.world_mut().register_component::<DT::Component>();
		let want = self.world_mut().register_component::<Want<DT>>();
		self.world_mut().get_resource_or_init::<ComponentDataRegistry>().wants_by_component.entry(component).or_default().push((want, unwant::<DT>));
		self.add_message::<ComponentResolved>()
			.init_resource::<RemovedWants<DT>>()
			.register_consumer_single_cached::<DT, Want<DT>>()
			.add_observer(queue_removed_want::<DT>)
			.add_systems(PostUpdate, update_wants::<DT>.before(TransformSystems::Propagate))
	}
}
