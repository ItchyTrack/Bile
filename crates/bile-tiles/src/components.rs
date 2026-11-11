use std::collections::{HashMap, HashSet};

use bevy::{ecs::component::{ComponentId, ComponentIdFor}, prelude::*};
use bile_components::{ComponentData, ComponentResolved, WantCommands};

use crate::TileKey;

#[derive(Debug, Clone, Copy, Message)]
pub struct TileClassReady {
	pub entity: Entity,
	pub tile_entity: Entity,
	pub tile_key: TileKey,
	pub class: ComponentId,
}

#[derive(Debug, Component, Default)]
pub struct DesiredComponents {
	classes: HashMap<ComponentId, u32>,
	components_by_class: HashMap<ComponentId, HashMap<ComponentId, u32>>,
	finished_components: HashSet<ComponentId>,
	ready_classes: HashSet<ComponentId>,
}

pub trait DesireForClass {
	fn add_desired_component<DT: ComponentData>(&mut self, class_id: ComponentId, params: DT::Params) -> &mut Self;
	fn remove_desired_component<DT: ComponentData>(&mut self, class_id: ComponentId) -> &mut Self;
}

impl DesireForClass for EntityCommands<'_> {
	fn add_desired_component<DT: ComponentData>(&mut self, class_id: ComponentId, params: DT::Params) -> &mut Self {
		self.queue(move |mut entity: EntityWorldMut| {
			let component_id = entity.world_scope(|world| world.register_component::<DT::Component>());
			let Some(mut desired_components) = entity.get_mut::<DesiredComponents>() else { return };
			let count = desired_components.components_by_class.entry(class_id).or_default().entry(component_id).or_default();
			*count += 1;
			if *count == 1 {
				bile_components::want::<DT>(&mut entity, params);
			}
		})
	}
	fn remove_desired_component<DT: ComponentData>(&mut self, class_id: ComponentId) -> &mut Self {
		self.queue_silenced(move |mut entity: EntityWorldMut| {
			let component_id = entity.world_scope(|world| world.register_component::<DT::Component>());
			let Some(mut desired_components) = entity.get_mut::<DesiredComponents>() else { return };
			let Some(components_for_class) = desired_components.components_by_class.get_mut(&class_id) else {
				bevy::log::warn!("Negative want on component. This should not happen with correct usage.");
				return;
			};
			let Some(desired_count) = components_for_class.get_mut(&component_id) else {
				bevy::log::warn!("Negative want on component. This should not happen with correct usage.");
				return;
			};
			if *desired_count == 1 {
				if components_for_class.len() == 1 {
					desired_components.components_by_class.remove(&class_id);
				} else {
					components_for_class.remove(&component_id);
				}
				desired_components.release_finished(component_id);
				bile_components::unwant::<DT>(&mut entity);
			} else {
				*desired_count -= 1;
			}
		})
	}
}

impl DesiredComponents {
	pub fn is_component_desired(&self, component_id: ComponentId) -> bool {
		self.components_by_class.values().any(|components| components.contains_key(&component_id))
	}
	pub fn has_class(&self, class_id: ComponentId) -> bool {
		self.classes.contains_key(&class_id)
	}
	pub fn is_class_ready(&self, class_id: ComponentId) -> bool {
		self.ready_classes.contains(&class_id)
	}
	#[must_use = "the returned components must be unwanted"]
	pub(crate) fn remove_class_components(&mut self, class_id: ComponentId) -> Vec<ComponentId> {
		let components: Vec<_> = self.components_by_class.remove(&class_id).map(|components| components.into_keys().collect()).unwrap_or_default();
		for &component_id in &components {
			self.release_finished(component_id);
		}
		components
	}
	fn release_finished(&mut self, component_id: ComponentId) {
		if !self.is_component_desired(component_id) {
			self.finished_components.remove(&component_id);
		}
	}
	fn finish_component(&mut self, component_id: ComponentId) {
		if self.is_component_desired(component_id) {
			self.finished_components.insert(component_id);
		}
	}
	// returns if a update needs to be made. desired_delta > 0 means add. desired_delta < 0 means remove.
	pub(crate) fn update_class(&mut self, component_id: ComponentId, desired_delta: i32) -> bool {
		let Some(counter) = self.classes.get_mut(&component_id) else {
			if desired_delta > 0 {
				self.classes.insert(component_id, desired_delta as u32);
				return true;
			}
			bevy::log::warn!("Negative want on class. This should not happen with correct usage.");
			return false;
		};
		match (*counter as i32).cmp(&-desired_delta) {
			std::cmp::Ordering::Less => {
				bevy::log::warn!("Negative want on class. This should not happen with correct usage.");
			},
			std::cmp::Ordering::Equal => {
				self.classes.remove(&component_id);
				self.ready_classes.remove(&component_id);
				return true;
			},
			std::cmp::Ordering::Greater => {
				*counter = (*counter as i32 + desired_delta) as u32
			},
		};
		false
	}
	pub(crate) fn get_classes(&self) -> &HashMap<ComponentId, u32> {
		&self.classes
	}
	fn class_ready(&self, class_id: ComponentId) -> bool {
		self.components_by_class
			.get(&class_id)
			.is_none_or(|components| components.keys().all(|component_id| self.finished_components.contains(component_id)))
	}
	fn update_ready_classes(&mut self, newly_ready: &mut Vec<ComponentId>) {
		let ready: Vec<_> = self.classes.keys().copied().filter(|&class_id| self.class_ready(class_id)).collect();
		self.ready_classes.retain(|class_id| ready.contains(class_id));
		for class_id in ready {
			if self.ready_classes.insert(class_id) {
				newly_ready.push(class_id);
			}
		}
	}
}

pub(crate) fn remove_desired_components_for_removed_classes<T: Component>(
	mut commands: Commands,
	class_component_id: ComponentIdFor<T>,
	mut desired_components_managers: Query<&mut DesiredComponents>,
	mut removed_classes: RemovedComponents<T>
) {
	let class_component_id = class_component_id.get();
	for removed_class in removed_classes.read() {
		if let Ok(mut desired_components_manager) = desired_components_managers.get_mut(removed_class) {
			for component_id in desired_components_manager.remove_class_components(class_component_id) {
				commands.entity(removed_class).unwant_by_id(component_id);
			}
		}
	}
}

pub(crate) fn update_ready_classes(
	mut resolved_components: MessageReader<ComponentResolved>,
	mut tiles: ParamSet<(
		Query<&mut DesiredComponents>,
		Query<(Entity, &mut DesiredComponents, &TileKey, &ChildOf), Changed<DesiredComponents>>,
	)>,
	mut ready_messages: MessageWriter<TileClassReady>,
) {
	{
		let mut tiles = tiles.p0();
		for resolved in resolved_components.read() {
			let Ok(mut desired_components) = tiles.get_mut(resolved.entity) else { continue };
			desired_components.finish_component(resolved.component);
		}
	}

	let mut newly_ready = Vec::new();
	for (tile_entity, mut desired_components, &tile_key, child_of) in &mut tiles.p1() {
		desired_components.bypass_change_detection().update_ready_classes(&mut newly_ready);
		for class in newly_ready.drain(..) {
			ready_messages.write(TileClassReady { entity: child_of.parent(), tile_entity, tile_key, class });
		}
	}
}
