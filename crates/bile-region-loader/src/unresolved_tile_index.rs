use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use bile_math::NonZeroRegion;

use bile_tiles::TileIndex;
use crate::types::EntityTileKey;


#[derive(Debug, Clone, Default)]
struct EntityTiles {
	present: HashSet<EntityTileKey>,
	index: TileIndex<EntityTileKey>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct UnresolvedTileIndex {
	by_entity: HashMap<Entity, EntityTiles>,
}

impl UnresolvedTileIndex {
	pub(crate) fn contains(&self, key: EntityTileKey) -> bool {
		self.by_entity.get(&key.entity).is_some_and(|e| e.present.contains(&key))
	}

	pub(crate) fn insert(&mut self, key: EntityTileKey) {
		let entity = self.by_entity.entry(key.entity).or_default();
		if entity.present.insert(key) {
			entity.index.insert(key);
		}
	}

	pub(crate) fn remove(&mut self, key: EntityTileKey) -> bool {
		let Some(entity) = self.by_entity.get_mut(&key.entity) else { return false };
		if !entity.present.remove(&key) {
			return false;
		}
		entity.index.remove(key);
		if entity.present.is_empty() {
			self.by_entity.remove(&key.entity);
		}
		true
	}

	pub(crate) fn keys(&self) -> Vec<EntityTileKey> {
		self.by_entity.values().flat_map(|e| e.present.iter().copied()).collect()
	}

	pub(crate) fn for_each_in_region(
		&self,
		entity: Entity,
		region: NonZeroRegion,
		skip_lod: Option<u8>,
		mut f: impl FnMut(EntityTileKey),
	) {
		let Some(entity_index) = self.by_entity.get(&entity) else { return };
		entity_index.index.for_each_overlapping(region, |key| {
			if Some(key.tile_key.lod) == skip_lod {
				return;
			}
			f(key);
		});
	}
}