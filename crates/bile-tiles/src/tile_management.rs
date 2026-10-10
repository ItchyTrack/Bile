use std::collections::HashMap;

use bevy::prelude::*;
use bevy::ecs::component::ComponentId;
use bile_math::region::NonZeroRegion;

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
	mut tile_instances: Query<&mut TileInstances, Without<DesiredComponents>>,
	mut desired_components: Query<&mut DesiredComponents, Without<TileInstances>>,
) {
	for mut tile_instance in &mut tile_instances {
		for tile_class_addition in tile_instance.tile_class_additions.drain() {
			// tile_instance.key_to_entity.get()

		}
	}
}
