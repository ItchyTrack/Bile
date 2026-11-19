use std::collections::{HashMap, HashSet};

use bevy::{
	ecs::{
		component::{ComponentId, ComponentIdFor},
		system::StaticSystemParam,
	},
	prelude::*,
};
use bile_data::{
	BileDataSchedule, BileDataSystemSet, BileSetDataParts, DataParts, DataType, DirtyState, Part, Request,
	simple::{self, Requests, SimpleFulfiller, SimpleFulfillerStorage, SimpleRequester, SimpleRequesterStorage, Sources},
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
pub trait ComponentRequesterStorage<DT: ComponentData>: Part {
	fn want(&mut self, params: &DT::Params, entity: Entity);
	/// `data` is what the component of the entity was.
	fn unwant(&mut self, params: &DT::Params, entity: Entity, data: DT::Data);
	/// The wanted data that is not up to date.
	fn take_unsatisfied(&mut self) -> Vec<DT::Params>;
	/// The fulfiller could not get the data right away, it comes later.
	fn requested(&mut self, params: &DT::Params);
}

/// Requests the data that entities want as a component, based on their `Want`. It does not provide to a node, the
/// storage puts the data in the components.
pub struct ComponentRequester;

impl ComponentRequester {
	pub fn setup<DT, R, S, F>(app: &mut App)
	where DT: ComponentData, R: Part, S: ComponentRequesterStorage<DT>, F: simple::Fulfiller<DT, S>,
	{
		app.add_observer(unwant_removed::<DT, R, S, F>).add_systems(BileDataSchedule, request_wanted::<DT, R, S, F>.in_set(BileDataSystemSet::Request));
	}
}

fn request_wanted<DT, R, S, F>(
	mut parts: Query<&mut DataParts<DT, R, S, F>>,
	mut param: StaticSystemParam<F::Param>,
	wants: Query<(Entity, &Want<DT>), Added<Want<DT>>>,
) where DT: ComponentData, R: Part, S: ComponentRequesterStorage<DT>, F: simple::Fulfiller<DT, S>,
{
	for mut parts in &mut parts {
		let DataParts { storage, fulfiller, .. } = &mut *parts;
		for (entity, want) in &wants {
			storage.want(&want.params, entity);
		}
		for params in storage.take_unsatisfied() {
			if !fulfiller.fulfill(storage, &mut param, Request::new(params.clone(), DirtyState::UpToDate)) {
				storage.requested(&params);
			}
		}
	}
}

fn unwant_removed<DT, R, S, F>(remove: On<Remove<Want<DT>>>, wants: Query<(&Want<DT>, Option<&DT::Component>)>, mut parts: Query<&mut DataParts<DT, R, S, F>>)
where DT: ComponentData, R: Part, S: ComponentRequesterStorage<DT>, F: simple::Fulfiller<DT, S>,
{
	let (want, component) = wants.get(remove.entity).expect("The Want is only removed after this");
	for mut parts in &mut parts {
		parts.storage.unwant(&want.params, remove.entity, component.cloned());
	}
}

// ============================ Component Storage ============================

/// Keeps wanted data only in the components of the entities that want it, and other data in a column for the nodes,
/// so it is never stored twice. The data is moved to where it is kept at the end of the bile update. Nodes can request
/// the data either way.
pub struct ComponentStorage<DT: ComponentData> {
	entries: HashMap<DT::Params, Entry<DT>>,
	requests: Requests<DT>,
	sources: Sources<DT>,
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
	fn default() -> Self {
		Self { entries: HashMap::default(), requests: Requests::default(), sources: Sources::default(), moved: HashSet::default(), unsatisfied: HashSet::default() }
	}
}

impl<DT: ComponentData> Default for Entry<DT> {
	fn default() -> Self { Self { wanting: HashSet::default(), holders: HashSet::default(), stored: None, pending: false } }
}

impl<DT: ComponentData> ComponentStorage<DT> {
	pub fn setup<R: Part, F: Part>(app: &mut App) {
		app.add_systems(BileDataSchedule, update_components::<DT, R, F>.in_set(BileDataSystemSet::Store));
	}
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
	// Drops the entry if nothing wants or requests it anymore.
	fn release_unused(&mut self, params: &DT::Params) {
		if self.entries.get(params).is_some_and(|entry| entry.wanting.is_empty()) && !self.requests.is_requested(params) {
			self.entries.remove(params);
			self.sources.release(params);
		}
	}
}

impl<DT: ComponentData> SimpleRequesterStorage<DT> for ComponentStorage<DT> {
	type Param = Query<'static, 'static, &'static DT::Component>;

	fn data(&self, components: &Query<&DT::Component>, params: &DT::Params) -> Option<(DirtyState, DT::Data)> { self.get(components, params) }
	fn requests(&mut self) -> &mut Requests<DT> { &mut self.requests }
	fn release(&mut self, params: &DT::Params) { self.release_unused(params); }
}

impl<DT: ComponentData> SimpleFulfillerStorage<DT> for ComponentStorage<DT> {
	type Param = Query<'static, 'static, &'static DT::Component>;

	fn data(&self, components: &Query<&DT::Component>, params: &DT::Params) -> Option<(DirtyState, DT::Data)> { self.get(components, params) }
	fn dirty_state(&self, params: &DT::Params) -> Option<DirtyState> { self.entries.get(params)?.stored.as_ref().map(|(dirty_state, _)| *dirty_state) }
	fn sources(&mut self) -> &mut Sources<DT> { &mut self.sources }
	fn wanted(&self, params: &DT::Params) -> bool {
		self.requests.is_waiting(params) || self.entries.get(params).is_some_and(|entry| !entry.wanting.is_empty())
	}
	fn is_requested(&self, params: &DT::Params, allowed_tick: &DirtyState) -> bool {
		self.requests.is_waiting_for(params, allowed_tick) || self.entries.get(params).is_some_and(|entry| entry.pending)
	}
	fn store(&mut self, params: &DT::Params, dirty_state: DirtyState, data: Option<DT::Data>) {
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
	fn unwant(&mut self, params: &DT::Params, entity: Entity, data: DT::Data) {
		let Some(entry) = self.entries.get_mut(params) else { return; }; // removed before it was requested
		entry.wanting.remove(&entity);
		// The last holder takes the data with it, so it is moved back to the column.
		if entry.holders.remove(&entity) && entry.holders.is_empty() && let Some((_, location @ Location::Components)) = &mut entry.stored {
			*location = Location::Column(data);
			self.moved.insert(params.clone());
		}
		self.release_unused(params);
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
fn update_components<DT: ComponentData, R: Part, F: Part>(
	mut commands: Commands,
	component: ComponentIdFor<DT::Component>,
	mut parts: Query<&mut DataParts<DT, R, ComponentStorage<DT>, F>>,
	components: Query<&DT::Component>,
	mut wants: Query<&mut Want<DT>>,
	mut resolved: MessageWriter<ComponentResolved>,
) {
	for mut parts in &mut parts {
		let storage = &mut parts.storage;
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
	/// Moves `DT` around with the component storage, so entities can want it as a component.
	fn add_component_data<DT: ComponentData>(&mut self) -> &mut Self;
}

impl AddComponentData for App {
	fn add_component_data<DT: ComponentData>(&mut self) -> &mut Self {
		let component = self.world_mut().register_component::<DT::Component>();
		let want = self.world_mut().register_component::<Want<DT>>();
		self.world_mut().get_resource_or_init::<ComponentDataRegistry>().wants_by_component.entry(component).or_default().push((want, unwant::<DT>));
		self.add_message::<ComponentResolved>().set_data_parts::<DT, SimpleRequester, ComponentStorage<DT>, SimpleFulfiller>();
		ComponentRequester::setup::<DT, SimpleRequester, ComponentStorage<DT>, SimpleFulfiller>(self);
		ComponentStorage::<DT>::setup::<SimpleRequester, SimpleFulfiller>(self);
		self
	}
}
