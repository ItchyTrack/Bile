use std::collections::{HashMap, HashSet};

use bevy::{ecs::component::{ComponentId, ComponentIdFor}, prelude::*, transform::TransformSystems};
use bile_data::{DataType, DirtyState, Request, simple};
use bevy_trait_query::RegisterExt;

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
	resolved: bool,
}

impl<DT: ComponentData> Want<DT> {
	pub fn params(&self) -> &DT::Params { &self.params }
	pub fn is_resolved(&self) -> bool { self.resolved }
}

#[derive(Default)]
struct Wanted {
	entities: HashSet<Entity>,
	source_entity: Option<Entity>, // None until the requested node passed the data
}

// The storage of the wanted data. The data is only in the components of the entities that want it.
// It is a sink, so it does not impl the API: nothing can request from it.
#[derive(Resource)]
struct ComponentStorage<DT: ComponentData> {
	wanted: HashMap<DT::Params, Wanted>,
	to_request: HashSet<DT::Params>,
	passed: Vec<(DT::Params, DT::Data)>,
	data_to_drop: HashMap<DT::Params, HashSet<Entity>>,
}

impl<DT: ComponentData> Default for ComponentStorage<DT> {
	fn default() -> Self {
		Self {
			wanted: HashMap::default(),
			to_request: HashSet::default(),
			passed: Vec::default(),
			data_to_drop: HashMap::default(),
		}
	}
}

impl<DT: ComponentData> simple::DataStorage<DT> for ComponentStorage<DT> {
	fn pass_data(&mut self, source_entity: Entity, params: &DT::Params, dirty_state: DirtyState, data: &DT::Data) {
		let Some(wanted) = self.wanted.get_mut(params) else {
			self.data_to_drop.entry(params.clone()).or_default().insert(source_entity);
			return;
		};
		if !dirty_state.satisfies(&DirtyState::UpToDate) { return; }
		if let Some(old_source_entity) = wanted.source_entity.replace(source_entity) && old_source_entity != source_entity {
			self.data_to_drop.entry(params.clone()).or_default().insert(old_source_entity);
		}
		self.passed.push((params.clone(), data.clone()));
	}
	fn dirty_data(&mut self, _data_consuming_nodes: &mut simple::DataConsumingNodes<DT>, params: &DT::Params, _last_clean_tick: u64, _diff: Option<&DT::Diff>) {
		if self.wanted.contains_key(params) {
			self.to_request.insert(params.clone());
		}
	}
	fn uses(&self, params: &DT::Params, source_entity: Entity) -> bool {
		self.wanted.get(params).is_some_and(|wanted| wanted.source_entity.is_none_or(|used| used == source_entity))
	}
	fn drop_data(&mut self) -> HashMap<DT::Params, HashSet<Entity>> { std::mem::take(&mut self.data_to_drop) }
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
	entity.insert(Want::<DT> { params, count: 1, resolved: false });
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

fn cancel_removed_want<DT: ComponentData>(remove: On<Remove<Want<DT>>>, wants: Query<&Want<DT>>, mut storage: ResMut<ComponentStorage<DT>>) {
	let params = &wants.get(remove.entity).expect("The Want is only removed after this").params;
	let storage = &mut *storage;
	let Some(wanted) = storage.wanted.get_mut(params) else { return; }; // removed before it was requested
	wanted.entities.remove(&remove.entity);
	if wanted.entities.is_empty() {
		storage.data_to_drop.entry(params.clone()).or_default().extend(wanted.source_entity);
		storage.wanted.remove(params);
	}
}

fn update_wants<DT: ComponentData>(
	mut commands: Commands,
	component: ComponentIdFor<DT::Component>,
	mut storage: ResMut<ComponentStorage<DT>>,
	mut data_producing_node: simple::DataProducingNodes<DT>,
	mut resolved: MessageWriter<ComponentResolved>,
	mut wants: ParamSet<(Query<(Entity, &Want<DT>), Added<Want<DT>>>, Query<&mut Want<DT>>)>,
) {
	let storage = &mut *storage;
	for (entity, want) in &wants.p0() {
		storage.wanted.entry(want.params.clone()).or_default().entities.insert(entity);
		storage.to_request.insert(want.params.clone());
	}
	for params in storage.to_request.drain() {
		let request = Request::new(params, DirtyState::UpToDate);
		let node_entity = simple::best_node(&data_producing_node, &request).expect("Data request failed to find node to fulfill the request. This should never happen!").1;
		data_producing_node.get_mut(node_entity).expect("This must exist as it was found in best_node").0.request(request);
	}
	for (params, data) in storage.passed.drain(..) {
		let Some(wanted) = storage.wanted.get(&params) else { continue };
		for &entity in &wanted.entities {
			match data.clone() {
				Some(data) => { commands.entity(entity).try_insert(data); }
				None => { commands.entity(entity).try_remove::<DT::Component>(); }
			}
			let mut wants = wants.p1();
			let mut want = wants.get_mut(entity).expect("The entity is no longer wanted when its Want is removed");
			if !want.resolved {
				want.resolved = true;
				resolved.write(ComponentResolved { entity, component: component.get() });
			}
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
			.init_resource::<ComponentStorage<DT>>()
			.register_component_as::<dyn simple::DataStorage<DT>, ComponentStorage<DT>>()
			.add_observer(cancel_removed_want::<DT>)
			.add_systems(PostUpdate, update_wants::<DT>.before(TransformSystems::Propagate))
	}
}
