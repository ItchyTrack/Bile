use std::{
	collections::{HashMap, HashSet},
	marker::PhantomData,
};

use bevy::{
	ecs::component::{ComponentId, ComponentIdFor},
	prelude::*,
};
use bevy_trait_query::RegisterExt;
use bile_data::{
	BileDataSchedule, BileDataSystemSet, DataType, DirtyState, Request,
	dyn_system_params::{DynParam, RegisterDynParam},
	simple_api::{DynStorageFulfillerParams, DynStorageRequesterParams, StorageFulfillerAPI, StorageFulfillerParams, StorageRequesterAPI, StorageRequesterParams},
	simple_storage::{Fulfillers, Requesters, StorageRequests},
};

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

// ============================ Wanting ============================

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

// ============================ Component Storage ============================

/// Keeps wanted data only in the components of the entities that want it, and other data in a column for the
/// requesters, so it is never stored twice. The data is moved to where it is kept at the end of the bile update.
/// Requesters can request the data either way. The wants are its own requests, it is their requester.
#[derive(Resource)]
pub struct ComponentStorage<DT: ComponentData> {
	entity: Entity, // The resource entity, the storage knows the wants by it
	entries: HashMap<DT::Params, Entry<DT>>,
	requests: StorageRequests<DT>,
	moved: HashSet<DT::Params>,       // the components are updated at the end of the bile update
	unsatisfied: HashSet<DT::Params>, // they may want data that is not up to date
	released: HashSet<DT::Params>,    // nothing wants them anymore, their request is cancelled next
}

struct Entry<DT: ComponentData> {
	wanting: HashSet<Entity>,
	holders: HashSet<Entity>, // the wanting entities whose component is the data, or older data
	stored: Option<(DirtyState, Location<DT>)>, // None until a node passed the data
}

enum Location<DT: ComponentData> {
	Column(DT::Data),
	Components,
}

impl<DT: ComponentData> Default for Entry<DT> {
	fn default() -> Self { Self { wanting: HashSet::default(), holders: HashSet::default(), stored: None } }
}

impl<DT: ComponentData> ComponentStorage<DT> {
	fn get(&self, components: &Query<&DT::Component>, params: &DT::Params) -> Option<(DirtyState, DT::Data)> {
		let entry = self.entries.get(params)?;
		let (dirty_state, location) = entry.stored.as_ref()?;
		let data = match location {
			Location::Column(data) => data.clone(),
			Location::Components => components.get(*entry.holders.iter().next()?).ok().cloned(),
		};
		Some((*dirty_state, data))
	}
	// The components keep the older data until the new data is up to date
	fn store(&mut self, params: &DT::Params, dirty_state: DirtyState, data: DT::Data) {
		self.entries.entry(params.clone()).or_default().stored = Some((dirty_state, Location::Column(data)));
		self.moved.insert(params.clone());
		self.unsatisfied.insert(params.clone());
	}
	fn want(&mut self, params: &DT::Params, entity: Entity) {
		self.entries.entry(params.clone()).or_default().wanting.insert(entity);
		self.moved.insert(params.clone());
		self.unsatisfied.insert(params.clone());
	}
	// `data` is what the component of the entity was
	fn unwant(&mut self, params: &DT::Params, entity: Entity, data: DT::Data) {
		let Some(entry) = self.entries.get_mut(params) else { return; };
		entry.wanting.remove(&entity);
		// The last holder takes the data with it, so it is moved back to the column.
		if entry.holders.remove(&entity) && entry.holders.is_empty() && let Some((_, location @ Location::Components)) = &mut entry.stored {
			*location = Location::Column(data);
		}
		if entry.wanting.is_empty() {
			self.released.insert(params.clone());
		}
	}
	// Drops the entry if nothing wants or requests it anymore
	fn release_unused(&mut self, params: &DT::Params) {
		if self.entries.get(params).is_some_and(|entry| entry.wanting.is_empty()) && !self.requests.is_requested(params) {
			self.entries.remove(params);
		}
	}
}

pub struct ComponentRequesterSystemParams<DT: ComponentData>(PhantomData<DT>);

impl<DT: ComponentData> DynParam<StorageRequesterParams<DT>> for ComponentRequesterSystemParams<DT> {
	type Param = (Fulfillers<'static, 'static, DT>, Query<'static, 'static, &'static DT::Component>);
}

pub struct ComponentFulfillerSystemParams<DT: ComponentData>(PhantomData<DT>);

impl<DT: ComponentData> DynParam<StorageFulfillerParams<DT>> for ComponentFulfillerSystemParams<DT> {
	type Param = Requesters<'static, 'static, DT>;
}

impl<DT: ComponentData> StorageRequesterAPI<DT> for ComponentStorage<DT> {
	fn request(
		&mut self,
		storage_params: &mut DynStorageRequesterParams<DT>,
		requester: Entity,
		request: &Request<DT>,
	) -> Option<(DT::Data, DirtyState)> {
		self.requests.request(requester, request);
		let (mut fulfillers, components) = storage_params.get::<ComponentRequesterSystemParams<DT>>();
		let stored = self.get(&components, &request.params);
		if let Some((dirty_state, data)) = &stored && *dirty_state >= request.allowed_tick {
			return Some((data.clone(), *dirty_state));
		}
		if self.requests.is_in_flight(request) { return None; }
		let stale = stored.and_then(|(_, data)| Some((data, self.requests.diffs(&request.params)?)));
		let Some(data) = fulfillers.request(request.clone(), stale) else {
			self.requests.passed_on(request);
			return None;
		};
		// The diffs were applied right away
		self.requests.pass_data(&request.params, DirtyState::UpToDate);
		self.store(&request.params, DirtyState::UpToDate, data.clone());
		Some((data, DirtyState::UpToDate))
	}

	fn cancel(
		&mut self,
		storage_params: &mut DynStorageRequesterParams<DT>,
		requester: Entity,
		params: &DT::Params,
		latest: DirtyState,
		oldest: Option<u64>,
	) {
		let cancel = self.requests.cancel(requester, params, latest, oldest);
		self.release_unused(params);
		if cancel.is_empty() { return; }
		let (mut fulfillers, _) = storage_params.get::<ComponentRequesterSystemParams<DT>>();
		for (latest, oldest) in cancel {
			fulfillers.cancel(params, latest, oldest);
		}
	}
}

impl<DT: ComponentData> StorageFulfillerAPI<DT> for ComponentStorage<DT> {
	fn pass_data(
		&mut self,
		storage_params: &mut DynStorageFulfillerParams<DT>,
		params: &DT::Params,
		dirty_state: DirtyState,
		data: DT::Data,
	) {
		if !self.requests.is_requested(params) { return; }
		if self.entries.get(params).and_then(|entry| entry.stored.as_ref()).is_some_and(|(stored, _)| *stored > dirty_state) { return; }
		// The wants are not requesters, they get the data at the end of the bile update
		let requesters = self.requests.pass_data(params, dirty_state);
		storage_params.get::<ComponentFulfillerSystemParams<DT>>().pass_data(requesters, params, dirty_state, &data);
		self.store(params, dirty_state, data);
	}

	fn dirty_data(
		&mut self,
		storage_params: &mut DynStorageFulfillerParams<DT>,
		params: &DT::Params,
		last_clean_tick: u64,
		diff: Option<&DT::Diff>,
	) {
		if let Some((dirty_state, _)) = self.entries.get_mut(params).and_then(|entry| entry.stored.as_mut()) {
			*dirty_state = (*dirty_state).min(DirtyState::Dirty { tick: last_clean_tick });
			self.unsatisfied.insert(params.clone());
		}
		let requesters = self.requests.dirty_data(params, diff);
		storage_params.get::<ComponentFulfillerSystemParams<DT>>().dirty_data(requesters, params, last_clean_tick, diff);
	}
}

fn cancel_unwanted<DT: ComponentData>(mut storage: ResMut<ComponentStorage<DT>>, mut storage_params: DynStorageRequesterParams<DT>) {
	let storage = &mut *storage;
	for params in std::mem::take(&mut storage.released) {
		// wanted again
		if storage.entries.get(&params).is_some_and(|entry| !entry.wanting.is_empty()) { continue; }
		storage.cancel(&mut storage_params, storage.entity, &params, DirtyState::UpToDate, None);
	}
}

// The wants keep their component up to date
fn request_wanted<DT: ComponentData>(
	mut storage: ResMut<ComponentStorage<DT>>,
	mut storage_params: DynStorageRequesterParams<DT>,
	wants: Query<(Entity, &Want<DT>), Added<Want<DT>>>,
) {
	let storage = &mut *storage;
	for (entity, want) in &wants {
		storage.want(&want.params, entity);
	}
	for params in std::mem::take(&mut storage.unsatisfied) {
		if storage.entries.get(&params).is_none_or(|entry| entry.wanting.is_empty()) { continue; }
		storage.request(&mut storage_params, storage.entity, &Request::new(params, DirtyState::UpToDate));
	}
}

fn unwant_removed<DT: ComponentData>(
	remove: On<Remove<Want<DT>>>,
	wants: Query<(&Want<DT>, Option<&DT::Component>)>,
	mut storage: ResMut<ComponentStorage<DT>>,
) {
	let (want, component) = wants.get(remove.entity).expect("The Want is only removed after this");
	storage.unwant(&want.params, remove.entity, component.cloned());
}

// Moves the up to date data that is wanted from the column into the components of the entities that want it, after
// everything else in the bile update.
fn update_components<DT: ComponentData>(
	mut commands: Commands,
	component: ComponentIdFor<DT::Component>,
	mut storage: ResMut<ComponentStorage<DT>>,
	components: Query<&DT::Component>,
	mut wants: Query<&mut Want<DT>>,
	mut resolved: MessageWriter<ComponentResolved>,
) {
	let storage = &mut *storage;
	for params in std::mem::take(&mut storage.moved) {
		let Some(entry) = storage.entries.get_mut(&params) else { continue; };
		if entry.wanting.is_empty() { continue; }
		let (data, entities): (DT::Data, Vec<Entity>) = match &mut entry.stored {
			Some((DirtyState::UpToDate, location @ Location::Column(_))) => {
				let Location::Column(data) = std::mem::replace(location, Location::Components) else { unreachable!() };
				entry.holders.clear();
				(data, entry.wanting.iter().copied().collect())
			}
			// the entities that want it anew
			Some((_, Location::Components)) => {
				let Some(&holder) = entry.holders.iter().next() else { continue; };
				(components.get(holder).ok().cloned(), entry.wanting.difference(&entry.holders).copied().collect())
			}
			_ => continue,
		};
		for entity in entities {
			match data.clone() {
				Some(data) => { commands.entity(entity).try_insert(data); }
				None => { commands.entity(entity).try_remove::<DT::Component>(); }
			}
			entry.holders.insert(entity);
			if let Ok(mut want) = wants.get_mut(entity) && !want.resolved {
				want.resolved = true;
				resolved.write(ComponentResolved { entity, component: component.get() });
			}
		}
	}
}

// ============================ Registering ============================

pub trait AddComponentData {
	/// Keeps `DT` in the component storage, so entities can want it as a component. Its nodes are simple nodes.
	fn add_component_data<DT: ComponentData>(&mut self) -> &mut Self;
}

impl AddComponentData for App {
	fn add_component_data<DT: ComponentData>(&mut self) -> &mut Self {
		let component = self.world_mut().register_component::<DT::Component>();
		let want = self.world_mut().register_component::<Want<DT>>();
		self.world_mut().get_resource_or_init::<ComponentDataRegistry>().wants_by_component.entry(component).or_default().push((want, unwant::<DT>));
		self.insert_resource(ComponentStorage::<DT> {
			entity: Entity::PLACEHOLDER,
			entries: HashMap::default(),
			requests: StorageRequests::default(),
			moved: HashSet::default(),
			unsatisfied: HashSet::default(),
			released: HashSet::default(),
		});
		let entity = self.world().resource_entity::<ComponentStorage<DT>>().expect("It was just inserted");
		self.world_mut().resource_mut::<ComponentStorage<DT>>().entity = entity;
		self.add_message::<ComponentResolved>()
			.register_component_as::<dyn StorageRequesterAPI<DT>, ComponentStorage<DT>>()
			.register_component_as::<dyn StorageFulfillerAPI<DT>, ComponentStorage<DT>>()
			.register_dyn_param::<StorageRequesterParams<DT>, ComponentRequesterSystemParams<DT>>()
			.register_dyn_param::<StorageFulfillerParams<DT>, ComponentFulfillerSystemParams<DT>>()
			.add_observer(unwant_removed::<DT>)
			.add_systems(
				BileDataSchedule,
				(
					cancel_unwanted::<DT>.in_set(BileDataSystemSet::Cancel),
					request_wanted::<DT>.in_set(BileDataSystemSet::Request),
					update_components::<DT>.in_set(BileDataSystemSet::Store),
				),
			)
	}
}
