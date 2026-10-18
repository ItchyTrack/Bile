use std::collections::{HashMap, HashSet};

use bevy::{ecs::component::{ComponentId, ComponentIdFor}, prelude::*};

use crate::{TileKey, component_source::{ComponentSource, FinishedRequestMessage}};

/// Sent when every component desired by a class on a tile has finished its request. A class that
/// desires no components is ready as soon as the systems in [`crate::BileTilesSystemSet::UpdateWantedComponents`] have run.
/// Also resent when a class that is already ready is requested again on the same tile.
#[derive(Debug, Clone, Copy, Message)]
pub struct TileClassReady {
	/// The entity with the [`crate::TileInstances`] that owns the tile.
	pub entity: Entity,
	pub tile_entity: Entity,
	pub tile_key: TileKey,
	pub class: ComponentId,
}

#[derive(Debug, Component, Default)]
pub struct DesiredComponents {
	classes: HashMap<ComponentId, u32>,
	components: HashMap<ComponentId, u32>,
	components_by_class: HashMap<ComponentId, HashMap<ComponentId, u32>>,
	newly_desired_components: HashSet<ComponentId>,
	newly_undesired_components: HashSet<ComponentId>,
	finished_components: HashSet<ComponentId>,
	ready_classes: HashSet<ComponentId>,
}

impl DesiredComponents {
	pub fn add_desired_component(&mut self, class_id: ComponentId, component_id: ComponentId) {
		self.components_by_class.entry(class_id).or_default().entry(component_id).and_modify(|x| *x += 1).or_insert_with(|| {
			let desired_count = self.components.entry(component_id).or_default();
			*desired_count += 1;
			if *desired_count == 1 {
				self.newly_desired_components.insert(component_id);
				self.newly_undesired_components.remove(&component_id);
			}
			1
		});
	}
	pub fn remove_desired_component(&mut self, class_id: ComponentId, component_id: ComponentId) {
		let Some(components_for_class) = self.components_by_class.get_mut(&class_id) else {
			bevy::log::warn!("Negative want on component. This should not happen with correct usage.");
			return;
		};
		let Some(desired_count) = components_for_class.get_mut(&component_id) else {
			bevy::log::warn!("Negative want on component. This should not happen with correct usage.");
			return;
		};
		if *desired_count == 1 {
			if components_for_class.len() == 1 {
				self.components_by_class.remove(&class_id);
			} else {
				components_for_class.remove(&component_id);
			}
			self.release_component(component_id);
		} else {
			*desired_count -= 1;
		}
	}
	pub fn is_component_desired(&self, component_id: ComponentId) -> bool {
		self.components.contains_key(&component_id)
	}
	pub fn has_class(&self, class_id: ComponentId) -> bool {
		self.classes.contains_key(&class_id)
	}
	/// If the class was ready the last time readiness was checked.
	pub fn is_class_ready(&self, class_id: ComponentId) -> bool {
		self.ready_classes.contains(&class_id)
	}
	// Called once per class when the last want for it is removed from the tile.
	fn release_component(&mut self, component_id: ComponentId) {
		let desired_count = self.components.get_mut(&component_id).expect("If it was in components_by_class then its is components");
		if *desired_count == 1 {
			self.components.remove(&component_id);
			self.finished_components.remove(&component_id);
			self.newly_desired_components.remove(&component_id);
			self.newly_undesired_components.insert(component_id);
		} else {
			*desired_count -= 1;
		}
	}
	pub(crate) fn remove_class_components(&mut self, class_id: ComponentId) {
		let Some(desired_components) = self.components_by_class.remove(&class_id) else { return; };
		for component_id in desired_components.into_keys() {
			self.release_component(component_id);
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
	fn finish_component(&mut self, component_id: ComponentId) {
		if self.components.contains_key(&component_id) {
			self.finished_components.insert(component_id);
		}
	}
	fn class_ready(&self, class_id: ComponentId) -> bool {
		self.components_by_class
			.get(&class_id)
			.is_none_or(|components| components.keys().all(|component_id| self.finished_components.contains(component_id)))
	}
	// Pushes classes that became ready since the last call.
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
	class_component_id: ComponentIdFor<T>,
	mut desired_components_managers: Query<&mut DesiredComponents>,
	mut removed_classes: RemovedComponents<T>
) {
	let class_component_id = class_component_id.get();
	for removed_class in removed_classes.read() {
		if let Ok(mut desired_components_manager) = desired_components_managers.get_mut(removed_class) {
			desired_components_manager.remove_class_components(class_component_id);
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
	mut component_sources: Query<(Entity, &mut dyn ComponentSource)>,
) {
	for (mut desired_components_manager, child_of, tile_entity, &tile_key) in &mut desired_components_managers {
		if desired_components_manager.newly_desired_components.is_empty() {
			continue;
		}
		for component_id in desired_components_manager.newly_desired_components.drain() {
			// Pick the cheapest source that can provide the component.
			let mut best: Option<(Entity, usize, f32)> = None;
			for (source_entity, sources) in component_sources.iter_mut() {
				for (index, source) in sources.iter().enumerate() {
					let Some(cost) = source.component_cost(child_of.parent(), tile_key, component_id) else { continue };
					if best.is_none_or(|(_, _, best_cost)| cost < best_cost) {
						best = Some((source_entity, index, cost));
					}
				}
			}
			let Some((source_entity, index, _)) = best else {
				bevy::log::warn!("No ComponentSource can provide component {component_id:?} for tile {tile_key:?}");
				continue;
			};
			let (_, mut sources) = component_sources.get_mut(source_entity).unwrap();
			sources.iter_mut().nth(index).unwrap().request_component(child_of.parent(), tile_entity, tile_key, component_id);
		}
	}
}

pub(crate) fn update_ready_classes(
	mut finished_requests: MessageReader<FinishedRequestMessage>,
	mut tiles: ParamSet<(
		Query<&mut DesiredComponents>,
		Query<(Entity, &mut DesiredComponents, &TileKey, &ChildOf), Changed<DesiredComponents>>,
	)>,
	mut ready_messages: MessageWriter<TileClassReady>,
) {
	{
		let mut tiles = tiles.p0();
		for finished_request in finished_requests.read() {
			let Ok(mut desired_components) = tiles.get_mut(finished_request.tile_entity) else { continue };
			desired_components.finish_component(finished_request.component_id);
		}
	}

	let mut newly_ready = Vec::new();
	for (tile_entity, mut desired_components, &tile_key, child_of) in &mut tiles.p1() {
		// Readiness is cached state, so updating it should not count as a change.
		desired_components.bypass_change_detection().update_ready_classes(&mut newly_ready);
		for class in newly_ready.drain(..) {
			ready_messages.write(TileClassReady { entity: child_of.parent(), tile_entity, tile_key, class });
		}
	}
}
