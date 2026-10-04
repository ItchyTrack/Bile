use std::collections::{HashMap, HashSet};

use bevy::{ecs::component::{ComponentId, ComponentIdFor}, prelude::*};

use crate::{TileKey, component_source::ComponentSource};

#[derive(Debug, Component, Default)]
pub struct DesiredComponents {
	components: HashMap<ComponentId, u32>,
	components_by_class: HashMap<ComponentId, HashMap<ComponentId, u32>>,
	newly_desired_components: HashSet<ComponentId>,
	newly_undesired_components: HashSet<ComponentId>,
}

impl DesiredComponents {
	pub fn add_desired_component(&mut self, class_id: ComponentId, component_id: ComponentId) {
		self.components_by_class.entry(class_id).or_default().entry(component_id).and_modify(|x| *x += 1).or_insert_with(|| {
			*self.components.entry(component_id).or_default() += 1;
			self.newly_desired_components.insert(component_id);
			self.newly_undesired_components.remove(&component_id);
			1
		});
	}
	pub fn remove_desired_component(&mut self, class_id: ComponentId, component_id: ComponentId) {
		let Some(components_for_class) = self.components_by_class.get_mut(&class_id) else { return; };
		let Some(desired_count) = components_for_class.get_mut(&component_id) else { return; };
		if *desired_count == 1 {
			if components_for_class.len() == 1 {
				self.components_by_class.remove(&class_id);
			} else {
				components_for_class.remove(&component_id);
			}
			let desired_count = self.components.get_mut(&component_id).expect("If it was in components_by_class then its is components");
			if *desired_count == 1 {
				self.components.remove(&component_id);
				self.newly_desired_components.remove(&component_id);
				self.newly_undesired_components.insert(component_id);
			} else {
				*desired_count -= 1;
			}
		} else {
			*desired_count -= 1;
		}
	}
}

pub(crate) fn remove_desired_components_for_removed_classes<T: Component>(
	class_component_id: ComponentIdFor<T>,
	mut desired_components_managers: Query<&mut DesiredComponents>,
	mut removed_classes: RemovedComponents<T>
) {
	let class_component_id = class_component_id.get();
	for removed_class in removed_classes.read() {
		if let Ok(desired_components_manager) = &mut desired_components_managers.get_mut(removed_class) {
			let Some(desired_components) = desired_components_manager.components_by_class.remove(&class_component_id) else { continue; };
			for desired_component in desired_components {
				let desired_count = desired_components_manager.components.get_mut(&desired_component.0).expect("If it was in components_by_class then its is components");
				if *desired_count == 1 { // do removal
					desired_components_manager.components.remove(&desired_component.0);
					desired_components_manager.newly_desired_components.remove(&desired_component.0);
					desired_components_manager.newly_undesired_components.insert(desired_component.0);
				} else { // still wanted
					*desired_count -= 1;
				}
			}
		}
	}
}

pub(crate) fn remove_newly_undesired_components(
	mut commands: Commands,
	mut desired_components_managers: Query<(Entity, &mut DesiredComponents)>,
) {
	for (entity, mut desired_components_manager) in &mut desired_components_managers {
		if desired_components_manager.newly_undesired_components.is_empty() {
			continue;
		}

		let mut entity_commands = commands.entity(entity);
		for component_id in desired_components_manager.newly_undesired_components.drain() {
			entity_commands.remove_by_id(component_id);
		}
	}
}

pub(crate) fn request_newly_desired(
	mut desired_components_managers: Query<(&mut DesiredComponents, &ChildOf, Entity, &TileKey)>,
	mut component_sources: Query<&mut dyn ComponentSource>,
) {
	for (mut desired_components_manager, child_of, tile_entity, &tile_key) in &mut desired_components_managers {
		for component_id in desired_components_manager.newly_desired_components.drain() {
			for mut component_source in component_sources.iter_mut().flatten() {
				if component_source.component_cost(child_of.parent(), tile_key, component_id).is_some() {
					component_source.request_component(child_of.parent(), tile_entity, tile_key, component_id);
				}
			}
		}
	}
}
