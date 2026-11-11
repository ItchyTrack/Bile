use std::marker::PhantomData;

use bevy::prelude::*;
use bile_data::{DataRequester, DataType};
use bile_scene::NodeBounds;
use bile_tiles::TileKey;

use crate::{RegionLoaderComponent, RegionUpdated, TileContent};

#[derive(Resource)]
pub(crate) struct RemovedLoaderTiles<DT> {
	tiles: Vec<(Entity, TileKey)>,
	marker: PhantomData<fn() -> DT>,
}

impl<DT> Default for RemovedLoaderTiles<DT> {
	fn default() -> Self {
		Self { tiles: Vec::new(), marker: PhantomData }
	}
}

pub(crate) fn update_loaders<DT: DataType<Params = TileKey>>(
	mut commands: Commands,
	mut requester: DataRequester<DT>,
	mut region_updates: MessageReader<RegionUpdated>,
	mut removed: ResMut<RemovedLoaderTiles<DT>>,
	mut loaders: Query<(Entity, &mut RegionLoaderComponent<DT>, &NodeBounds)>,
) where DT::Data: TileContent {
	for (entity, key) in removed.tiles.drain(..) {
		requester.cancel(&key, entity);
	}
	for update in region_updates.read() {
		let Ok((_, mut loader, node_bounds)) = loaders.get_mut(update.entity) else { continue };
		loader.region_updated(node_bounds, update.region);
	}
	for (entity, mut loader, node_bounds) in &mut loaders {
		loader.update(node_bounds);
		loader.flush(entity, &mut commands, &mut requester);
	}
}

pub(crate) fn release_removed_loader<DT: DataType<Params = TileKey>>(
	remove: On<Remove, RegionLoaderComponent<DT>>,
	mut commands: Commands,
	mut removed: ResMut<RemovedLoaderTiles<DT>>,
	mut loaders: Query<&mut RegionLoaderComponent<DT>>,
) {
	let Ok(mut loader) = loaders.get_mut(remove.entity) else { return };
	loader.clear();
	for (key, resolution) in loader.take_released() {
		removed.tiles.push((remove.entity, key));
		if let Some(tile_entity) = resolution.entity() {
			commands.entity(tile_entity).try_despawn();
		}
	}
}
