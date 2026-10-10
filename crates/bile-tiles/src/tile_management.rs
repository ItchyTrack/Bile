use std::collections::HashMap;

use bevy::prelude::*;
use bevy::ecs::component::ComponentId;
use bile_math::NonZeroRegion;

use crate::{DesiredComponents, TileKey, TileKeyMap};

#[derive(Component, Default, Debug)]
pub struct TileInstances {
	tile_class_additions: HashMap<TileKey, HashMap<ComponentId, i32>>,
	key_to_entity: TileKeyMap<Entity>,
}

impl TileInstances {
	pub fn add_class(&mut self, tile_key: TileKey, component_id: ComponentId) {
		let Some(components) = self.tile_class_additions.get_mut(&tile_key) else {
			self.tile_class_additions.insert(tile_key, HashMap::from([(component_id, 1)]));
			return;
		};

		let Some(counter) = components.get_mut(&component_id) else {
			components.insert(component_id, 1);
			return;
		};

		if *counter != -1 {
			*counter += 1;
		} else if components.len() == 1 {
			self.tile_class_additions.remove(&tile_key);
		} else {
			components.remove(&component_id);
		}
	}

	pub fn remove_class(&mut self, tile_key: TileKey, component_id: ComponentId) {
		let Some(components) = self.tile_class_additions.get_mut(&tile_key) else {
			self.tile_class_additions.insert(tile_key, HashMap::from([(component_id, -1)]));
			return;
		};

		let Some(counter) = components.get_mut(&component_id) else {
			components.insert(component_id, -1);
			return;
		};

		if *counter != 1 {
			*counter -= 1;
		} else if components.len() == 1 {
			self.tile_class_additions.remove(&tile_key);
		} else {
			components.remove(&component_id);
		}
	}
	pub fn get(&mut self, tile_key: TileKey) -> Option<&Entity> {
		self.key_to_entity.get(tile_key)
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
	mut tile_instances: Query<&mut TileInstances, Without<DesiredComponents>>,
	mut desired_components: Query<&mut DesiredComponents, Without<TileInstances>>,
) {
	for mut tile_instance in &mut tile_instances {
		for tile_class_addition in std::mem::replace(&mut tile_instance.tile_class_additions, default()) {
			if let Some(entity) = tile_instance.get(tile_class_addition.0) {
				let Ok(mut desired_component) = desired_components.get_mut(*entity) else {
					bevy::log::error!("desired_component was not found for entity {}", entity);
					continue;
				};
				let mut entity_commands = commands.entity(*entity);
				for class_addition in tile_class_addition.1 {
					if desired_component.update_class(class_addition.0, class_addition.1) {
						if class_addition.1 > 0 {
							unsafe { entity_commands.insert_by_id(class_addition.0, ()); }
						} else {
							entity_commands.remove_by_id(class_addition.0);
						}
					}
				}
			} else {
				let mut desired_component = DesiredComponents::default();
				for class_addition in tile_class_addition.1 {
					desired_component.update_class(class_addition.0, class_addition.1);
				}
				if desired_component.get_classes().is_empty() { continue };

				let mut entity_commands = commands.spawn(tile_class_addition.0);
				for component_id in desired_component.get_classes().keys() {
					unsafe { entity_commands.insert_by_id(*component_id, ()); }
				}
				entity_commands.insert(desired_component);
			}
		}
	}
}
