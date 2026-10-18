use bevy::prelude::*;
use bile_scene::NodeBounds;
use bile_tiles::{TileClassReady, TileInstances};

use crate::{RegionLoaderComponent, RegionUpdated};

pub(crate) fn receive_ready_classes<M: 'static>(
	mut ready_classes: MessageReader<TileClassReady>,
	mut loaders: Query<(&mut RegionLoaderComponent<M>, &mut TileInstances)>,
) {
	for ready in ready_classes.read() {
		let Ok((mut loader, mut tile_instances)) = loaders.get_mut(ready.entity) else { continue };
		loader.tile_class_ready(&mut tile_instances, ready.tile_key, ready.class, ready.tile_entity);
	}
}

pub(crate) fn receive_region_updates<M: 'static>(
	mut region_updates: MessageReader<RegionUpdated>,
	mut loaders: Query<(&mut RegionLoaderComponent<M>, &mut TileInstances, &NodeBounds)>,
) {
	for update in region_updates.read() {
		let Ok((mut loader, mut tile_instances, node_bounds)) = loaders.get_mut(update.entity) else { continue };
		loader.region_updated(&mut tile_instances, node_bounds, update.region);
	}
}

pub(crate) fn update_requests<M: 'static>(
	mut loaders: Query<(&mut RegionLoaderComponent<M>, &mut TileInstances, &NodeBounds)>,
) {
	for (mut loader, mut tile_instances, node_bounds) in &mut loaders {
		if !loader.has_pending_regions() { continue; }
		loader.update(&mut tile_instances, node_bounds);
	}
}

pub(crate) fn release_removed_loader<M: 'static>(
	remove: On<Remove, RegionLoaderComponent<M>>,
	mut loaders: Query<(&mut RegionLoaderComponent<M>, &mut TileInstances)>,
) {
	let Ok((mut loader, mut tile_instances)) = loaders.get_mut(remove.entity) else { return };
	loader.clear(&mut tile_instances);
}
