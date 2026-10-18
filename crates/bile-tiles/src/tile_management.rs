use std::collections::HashMap;

use bevy::prelude::*;
use bevy::ecs::component::ComponentId;
use bile_math::NonZeroRegion;

use crate::{CancelRequestMessage, DesiredComponents, TileClassReady, TileKey, TileKeyMap};

#[derive(Component, Default, Debug)]
pub struct TileInstances {
	tile_class_additions: HashMap<TileKey, HashMap<ComponentId, i32>>,
	added_classes: Vec<(TileKey, ComponentId)>,
	key_to_entity: TileKeyMap<Entity>,
}

impl TileInstances {
	pub fn add_class(&mut self, tile_key: TileKey, class: ComponentId) {
		self.added_classes.push((tile_key, class));
		let Some(classes) = self.tile_class_additions.get_mut(&tile_key) else {
			self.tile_class_additions.insert(tile_key, HashMap::from([(class, 1)]));
			return;
		};

		let Some(counter) = classes.get_mut(&class) else {
			classes.insert(class, 1);
			return;
		};

		if *counter != -1 {
			*counter += 1;
		} else if classes.len() == 1 {
			self.tile_class_additions.remove(&tile_key);
		} else {
			classes.remove(&class);
		}
	}

	pub fn remove_class(&mut self, tile_key: TileKey, class: ComponentId) {
		let Some(classes) = self.tile_class_additions.get_mut(&tile_key) else {
			self.tile_class_additions.insert(tile_key, HashMap::from([(class, -1)]));
			return;
		};

		let Some(counter) = classes.get_mut(&class) else {
			classes.insert(class, -1);
			return;
		};

		if *counter != 1 {
			*counter -= 1;
		} else if classes.len() == 1 {
			self.tile_class_additions.remove(&tile_key);
		} else {
			classes.remove(&class);
		}
	}
	pub fn get(&self, tile_key: TileKey) -> Option<Entity> {
		self.key_to_entity.get(tile_key).copied()
	}
	pub fn for_each_overlapping(&self, region: NonZeroRegion, f: impl FnMut(TileKey)) {
		self.key_to_entity.for_each_overlapping(region, f)
	}
	pub fn keys_covering_point(&self, point: IVec3) -> Vec<TileKey> {
		self.key_to_entity.keys_covering_point(point)
	}
}

pub fn modify_tile_classes(
	mut commands: Commands,
	mut tile_instances: Query<(Entity, &mut TileInstances), Without<DesiredComponents>>,
	mut desired_components: Query<&mut DesiredComponents, Without<TileInstances>>,
	mut ready_messages: MessageWriter<TileClassReady>,
	mut cancel_messages: MessageWriter<CancelRequestMessage>,
) {
	for (instances_entity, mut tile_instance) in &mut tile_instances {
		if tile_instance.tile_class_additions.is_empty() && tile_instance.added_classes.is_empty() {
			continue;
		}
		let tile_instance = &mut *tile_instance;
		for (tile_key, class_additions) in std::mem::take(&mut tile_instance.tile_class_additions) {
			if let Some(entity) = tile_instance.get(tile_key) {
				let Ok(mut desired_component) = desired_components.get_mut(entity) else {
					bevy::log::error!("desired_component was not found for entity {}", entity);
					continue;
				};
				let mut entity_commands = commands.entity(entity);
				for (class, delta) in class_additions {
					if desired_component.update_class(class, delta) {
						if delta > 0 {
							unsafe { entity_commands.insert_by_id(class, ()); }
						} else {
							desired_component.remove_class_components(class);
							entity_commands.remove_by_id(class);
						}
					}
				}
				if desired_component.get_classes().is_empty() {
					for component_id in desired_component.take_cancelled_requests(true) {
						cancel_messages.write(CancelRequestMessage { entity: instances_entity, tile_entity: entity, tile_key, component_id });
					}
					entity_commands.despawn();
					tile_instance.key_to_entity.remove(tile_key);
				}
			} else {
				let mut desired_component = DesiredComponents::default();
				for (class, delta) in class_additions {
					desired_component.update_class(class, delta);
				}
				if desired_component.get_classes().is_empty() { continue };

				let mut entity_commands = commands.spawn((tile_key, ChildOf(instances_entity)));
				for component_id in desired_component.get_classes().keys() {
					unsafe { entity_commands.insert_by_id(*component_id, ()); }
				}
				entity_commands.insert(desired_component);
				tile_instance.key_to_entity.insert(tile_key, entity_commands.id());
			}
		}

		for (tile_key, class) in std::mem::take(&mut tile_instance.added_classes) {
			let Some(entity) = tile_instance.get(tile_key) else { continue };
			let Ok(desired_component) = desired_components.get(entity) else { continue };
			if desired_component.has_class(class) && desired_component.is_class_ready(class) {
				ready_messages.write(TileClassReady { entity: instances_entity, tile_entity: entity, tile_key, class });
			}
		}
	}
}
