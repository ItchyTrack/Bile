use bevy::{ecs::{component::ComponentId, message::MessageReader}, prelude::*};
use bile_math::NonZeroRegion;
use bile_scene::NodeBounds;
use bile_tiles::{TileInstances, TileKey};

use crate::{
	lod_bands::{for_each_tile_in_bands, run_over_diff},
	region_loader::RegionLoader,
	tile_lifecycle::ResolvedTile
};

fn acquire_tile(
	tile_instances: &mut TileInstances,
	lifecycle: &mut crate::tile_lifecycle::TileLifecycle,
	requester_entity: Entity,
	tile_key: TileKey,
	class: ComponentId,
	priority: f32,
) {
	tile_instances.add_class(tile_key, class);
	let released = lifecycle.resolve(tile_key, ResolvedTile::Empty);
	for key in released { tile_instances.remove_class(tile_key, class); } // this class is wrong sometimes!
}

pub(crate) fn update_region_loader_requests(
	mut loaders: Query<(&mut RegionLoader, &mut TileInstances, &NodeBounds)>,
) {
	for (mut region_loader, mut tile_instances, node_bounds) in &mut loaders {
		if region_loader.class == region_loader.new_class {
			let Some(new_regions) = region_loader.new_regions.take() else { continue; };

			if let Some(class) = region_loader.class {
				run_over_diff(&region_loader.regions, &new_regions, node_bounds, |lod, min, is_added| {
					let tile_key = TileKey { region: NonZeroRegion::new(min, UVec3::splat(2u32.pow(lod as u32))).unwrap(), lod };
					if is_added {
						tile_instances.add_class(tile_key, class);
					} else {
						tile_instances.remove_class(tile_key, class);
					}
				});
			}

			region_loader.regions = new_regions;
		} else {
			let new_class = region_loader.new_class.take();
			let new_regions = region_loader.new_regions.take().unwrap_or_else(|| region_loader.regions.clone());

			if let Some(old_class) = region_loader.class {
				run_over_diff(&region_loader.regions, &[], node_bounds, |lod, min, _| {
					tile_instances.remove_class(TileKey { region: NonZeroRegion::new(min, UVec3::splat(2u32.pow(lod as u32))).unwrap(), lod }, old_class);
				});
			}
			if let Some(new_class) = new_class {
				run_over_diff(&[], &new_regions, node_bounds, |lod, min, _| {
					tile_instances.add_class(TileKey { region: NonZeroRegion::new(min, UVec3::splat(2u32.pow(lod as u32))).unwrap(), lod }, new_class);
				});
			}

			region_loader.regions = new_regions;
			region_loader.class = new_class;
		}
	}
}

pub(crate) fn receive_region_loader_results(
	mut results: MessageReader<TileLoadUpdate>,
	mut loaders: Query<&mut RegionLoader>,
	mut releaser: TileReleaser,
) {
	for result in results.read() {
		let Ok(mut loader) = loaders.get_mut(result.requester) else { continue };
		let key = TileKey { entity: result.entity, tile_key: result.key };
		if !loader.tiles.contains_source(key) { continue; }
		let resolution = match result.status {
			TileLoadStatus::Ready(entity) => ResolvedTile::Tile(entity),
			TileLoadStatus::Empty => ResolvedTile::Empty,
		};
		let released = loader.tiles.resolve(key, resolution);
		for key in released { releaser.release_tile(key.entity, result.requester, key.tile_key); }
	}
}

pub(crate) fn refresh_region_loader_visibility(
	mut availability_events: MessageReader<ChunkAvailabilityChanged>,
	mut cameras: Query<(Entity, &mut RegionLoader)>,
	mut node_bounds: Query<&NodeBounds>,
) {
	let availability_events: Vec<_> = availability_events.read().copied().collect();

	for (camera_entity, mut loader) in &mut cameras {
		let mut acquisitions = Vec::new();
		let mut releases = Vec::new();
		{
			let entitys = requester_streaming.p1();
			let mut changed = Vec::new();
			let mut acquire = Vec::new();
			let mut release = Vec::new();
			for event in &availability_events {
				match event.kind {
					ChunkAvailabilityChangeKind::BecamePresent => {
						let Some(bands) = loader.bands.get(&event.entity) else { continue };
						let Some(class) = loader.classes.get(&event.entity).copied() else { continue };
						let Ok((node_bounds)) = entitys.get(event.entity) else { continue };
						changed.clear();
						for_each_tile_in_bands(bands, event.region, |lod, min| {
							let key = TileKey::new(event.entity, class, lod, min);
							if !loader.tiles.contains_desired(key) && tile_has_present_source(entity_streaming, key) { changed.push(key); }
						});
						loader.tiles.apply_delta(&changed, &[], &mut acquire, &mut release);
						releases.extend(release.drain(..));
						acquisitions.extend(acquire.drain(..).map(|key| key));
					}
					ChunkAvailabilityChangeKind::BecameEmpty => {
						let Ok((entity_streaming, _)) = entitys.get(event.entity) else { continue };
						loader.tiles.desired_in_area(event.region, &mut changed);
						changed.retain(|&key| !tile_has_present_source(entity_streaming, key));
						loader.tiles.apply_delta(&[], &changed, &mut acquire, &mut release);
						releases.extend(release.drain(..));
					}
				}
			}
		}

		let mut requester = requester_streaming.p0();
		for key in releases { requester.release_tile(key.entity, camera_entity, key.tile_key); }
		for key in acquisitions {
			if !loader.tiles.contains_source(key) { continue; }
			acquire_tile(&mut requester, &mut loader.tiles, camera_entity, key, 0.0);
		}
	}
}
