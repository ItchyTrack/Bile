use std::{
	collections::{HashMap, HashSet},
	marker::PhantomData,
};

use bevy::{
	ecs::component::{ComponentId, ComponentIdFor},
	prelude::*,
};
use bile_data::{
	BileDataParts, BileDataSchedule, BileDataSystemSet, DataPart, DataType, DirtyState, Request, Storage,
	simple::{self, Fulfillers, Requester, Requests, SimpleFulfiller, SimpleFulfillerStorage, SimpleRequester, SimpleRequesterStorage},
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

// ============================ Component Requester ============================

/// What the component requester needs from a storage.
pub trait ComponentRequesterStorage<DT: ComponentData>: Storage<DT> {
	fn want(&mut self, params: &DT::Params, entity: Entity);
	/// `data` is what the component of the entity was. Returns true if the storage dropped the data.
	fn unwant(&mut self, params: &DT::Params, entity: Entity, data: DT::Data) -> bool;
	/// The wanted data that is not up to date.
	fn take_unsatisfied(&mut self) -> Vec<DT::Params>;
	/// The fulfillers could not get the data right away, it comes later.
	fn requested(&mut self, params: &DT::Params);
}

/// Requests the data that entities want as a component, based on their `Want`. It does not provide to a node, the
/// storage puts the data in the components.
#[derive(Component)]
pub struct ComponentRequester<DT: ComponentData, S> {
	released: Vec<DT::Params>, // dropped by an unwant, the fulfillers are told in the next request
	_marker: PhantomData<fn() -> S>,
}

impl<DT: ComponentData, S> Default for ComponentRequester<DT, S> {
	fn default() -> Self { Self { released: Vec::new(), _marker: PhantomData } }
}

// impl<DT: ComponentData, S: ComponentRequesterStorage<DT>> DataPart<DT> for ComponentRequester<DT, S> {
// 	type Storage = S;

// 	fn setup(app: &mut App) {
// 		simple::setup_requester::<DT, Self>(app);
// 		app.add_observer(unwant_removed::<DT, S>).add_systems(BileDataSchedule, request_wanted::<DT, S>.in_set(BileDataSystemSet::Request));
// 	}
// }

impl<DT: ComponentData, S: ComponentRequesterStorage<DT>> Requester<DT> for ComponentRequester<DT, S> {
	type Param = ();

	// The storage knows which components are no longer up to date.
	fn mark_dirty(&mut self, _storage: &mut S, _param: &mut (), _params: &DT::Params, _last_clean_tick: u64, _diff: Option<&DT::Diff>) {}
}

fn request_wanted<DT: ComponentData, S: ComponentRequesterStorage<DT>>(
	mut parts: Query<(&mut ComponentRequester<DT, S>, &mut S)>,
	mut fulfillers: Fulfillers<DT>,
	wants: Query<(Entity, &Want<DT>), Added<Want<DT>>>,
) {
	for (mut requester, mut storage) in &mut parts {
		for params in std::mem::take(&mut requester.released) {
			fulfillers.release(&mut *storage, &params);
		}
		for (entity, want) in &wants {
			storage.want(&want.params, entity);
		}
		for params in storage.take_unsatisfied() {
			if !fulfillers.fulfill(&mut *storage, Request::new(params.clone(), DirtyState::UpToDate)) {
				storage.requested(&params);
			}
		}
	}
}

fn unwant_removed<DT: ComponentData, S: ComponentRequesterStorage<DT>>(
	remove: On<Remove<Want<DT>>>,
	wants: Query<(&Want<DT>, Option<&DT::Component>)>,
	mut parts: Query<(&mut ComponentRequester<DT, S>, &mut S)>,
) {
	let (want, component) = wants.get(remove.entity).expect("The Want is only removed after this");
	for (mut requester, mut storage) in &mut parts {
		if storage.unwant(&want.params, remove.entity, component.cloned()) {
			requester.released.push(want.params.clone());
		}
	}
}

// ============================ Component Storage ============================

/// Keeps wanted data only in the components of the entities that want it, and other data in a column for the nodes,
/// so it is never stored twice. The data is moved to where it is kept at the end of the bile update. Nodes can request
/// the data either way.
#[derive(Component)]
pub struct ComponentStorage<DT: ComponentData> {
	entries: HashMap<DT::Params, Entry<DT>>,
	requests: Requests<DT>,
	moved: HashSet<DT::Params>,       // the components are updated at the end of the bile update
	unsatisfied: HashSet<DT::Params>, // they may want data that is not up to date
}

struct Entry<DT: ComponentData> {
	wanting: HashSet<Entity>,
	holders: HashSet<Entity>, // the wanting entities whose component is the data, out of date if it was discarded
	stored: Option<(DirtyState, Location<DT>)>, // None until a node passed the data
	pending: bool,            // the wanted data comes later
}

enum Location<DT: ComponentData> {
	Column(DT::Data),
	Components,
	Discarded, // out of date and not worth keeping
}

impl<DT: ComponentData> Default for ComponentStorage<DT> {
	fn default() -> Self { Self { entries: HashMap::default(), requests: Requests::default(), moved: HashSet::default(), unsatisfied: HashSet::default() } }
}

impl<DT: ComponentData> Default for Entry<DT> {
	fn default() -> Self { Self { wanting: HashSet::default(), holders: HashSet::default(), stored: None, pending: false } }
}

impl<DT: ComponentData> ComponentStorage<DT> {
	fn get(&self, components: &Query<&DT::Component>, params: &DT::Params) -> Option<(DirtyState, DT::Data)> {
		let entry = self.entries.get(params)?;
		let (dirty_state, location) = entry.stored.as_ref()?;
		let data = match location {
			Location::Column(data) => data.clone(),
			Location::Components => components.get(*entry.holders.iter().next()?).ok().cloned(),
			Location::Discarded => return None,
		};
		Some((*dirty_state, data))
	}
	// Drops the entry if nothing wants or requests it anymore. Returns true if it did.
	fn release_unused(&mut self, params: &DT::Params) -> bool {
		let unused = self.entries.get(params).is_some_and(|entry| entry.wanting.is_empty()) && !self.requests.is_requested(params);
		unused && self.entries.remove(params).is_some()
	}
}

impl<DT: ComponentData> Storage<DT> for ComponentStorage<DT> {
	fn setup(app: &mut App) { app.add_systems(BileDataSchedule, update_components::<DT>.in_set(BileDataSystemSet::Store)); }
}

impl<DT: ComponentData> SimpleRequesterStorage<DT> for ComponentStorage<DT> {
	type Param = Query<'static, 'static, &'static DT::Component>;

	fn data(&self, components: &Query<&DT::Component>, params: &DT::Params) -> Option<(DirtyState, DT::Data)> { self.get(components, params) }
	fn requests(&mut self) -> &mut Requests<DT> { &mut self.requests }
	fn release(&mut self, params: &DT::Params) -> bool { self.release_unused(params) }
}

impl<DT: ComponentData> SimpleFulfillerStorage<DT> for ComponentStorage<DT> {
	type Param = Query<'static, 'static, &'static DT::Component>;

	fn data(&self, components: &Query<&DT::Component>, params: &DT::Params) -> Option<(DirtyState, DT::Data)> { self.get(components, params) }
	fn dirty_state(&self, params: &DT::Params) -> Option<DirtyState> { self.entries.get(params)?.stored.as_ref().map(|(dirty_state, _)| *dirty_state) }
	fn wanted(&self, params: &DT::Params) -> bool {
		self.requests.is_waiting(params) || self.entries.get(params).is_some_and(|entry| !entry.wanting.is_empty())
	}
	fn is_requested(&self, params: &DT::Params, allowed_tick: &DirtyState) -> bool {
		self.requests.is_waiting_for(params, allowed_tick) || self.entries.get(params).is_some_and(|entry| entry.pending)
	}
	fn store(&mut self, params: &DT::Params, dirty_state: DirtyState, data: Option<DT::Data>) {
		if data.is_some() {
			self.requests.stored(params, dirty_state);
		}
		let entry = self.entries.entry(params.clone()).or_default();
		entry.pending = false;
		// Discarded data stays in the components until the new data is there.
		entry.stored = Some((dirty_state, data.map_or(Location::Discarded, Location::Column)));
		self.moved.insert(params.clone());
		self.unsatisfied.insert(params.clone());
	}
	fn mark_dirty(&mut self, params: &DT::Params, last_clean_tick: u64) -> bool {
		let Some((dirty_state, _)) = self.entries.get_mut(params).and_then(|entry| entry.stored.as_mut()) else { return false; };
		*dirty_state = DirtyState::Dirty { tick: last_clean_tick };
		self.unsatisfied.insert(params.clone());
		true
	}
}

impl<DT: ComponentData> ComponentRequesterStorage<DT> for ComponentStorage<DT> {
	fn want(&mut self, params: &DT::Params, entity: Entity) {
		self.entries.entry(params.clone()).or_default().wanting.insert(entity);
		self.moved.insert(params.clone());
		self.unsatisfied.insert(params.clone());
	}
	fn unwant(&mut self, params: &DT::Params, entity: Entity, data: DT::Data) -> bool {
		let Some(entry) = self.entries.get_mut(params) else { return false; }; // removed before it was requested
		entry.wanting.remove(&entity);
		// The last holder takes the data with it, so it is moved back to the column.
		if entry.holders.remove(&entity) && entry.holders.is_empty() && let Some((_, location @ Location::Components)) = &mut entry.stored {
			*location = Location::Column(data);
			self.moved.insert(params.clone());
		}
		self.release_unused(params)
	}
	fn take_unsatisfied(&mut self) -> Vec<DT::Params> {
		let entries = &self.entries;
		self.unsatisfied
			.drain()
			.filter(|params| {
				entries.get(params).is_some_and(|entry| {
					!entry.wanting.is_empty() && !matches!(entry.stored, Some((DirtyState::UpToDate, Location::Column(_) | Location::Components)))
				})
			})
			.collect()
	}
	fn requested(&mut self, params: &DT::Params) {
		if let Some(entry) = self.entries.get_mut(params) {
			entry.pending = true;
		}
	}
}

// Moves the up to date data that is wanted from the column into the components of the entities that want it, after
// everything else in the bile update.
fn update_components<DT: ComponentData>(
	mut commands: Commands,
	component: ComponentIdFor<DT::Component>,
	mut storages: Query<&mut ComponentStorage<DT>>,
	components: Query<&DT::Component>,
	mut wants: Query<&mut Want<DT>>,
	mut resolved: MessageWriter<ComponentResolved>,
) {
	for mut storage in &mut storages {
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
		self.add_message::<ComponentResolved>()
			.set_data_storage::<DT, ComponentStorage<DT>>()
			.add_data_part::<DT, ComponentRequester<DT, ComponentStorage<DT>>>()
			.add_data_part::<DT, SimpleRequester<DT, ComponentStorage<DT>>>()
			.add_data_part::<DT, SimpleFulfiller<DT, ComponentStorage<DT>>>()
	}
}
