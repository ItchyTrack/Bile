use bevy::{ecs::message::MessageReader, prelude::*};

use crate::{
	FreezeRegionLoader,
	region_loader::RegionLoader,
	lod_bands::for_each_tile_in_bands,
	tile_lifecycle::ResolvedTile,
	types::EntityTileKey,
};

fn acquire_tile(
	requester: &mut TileRequester,
	lifecycle: &mut crate::tile_lifecycle::TileLifecycle,
	requester_entity: Entity,
	key: EntityTileKey,
	priority: f32,
) {
	if requester.fetch_tile(key.entity, requester_entity, key.tile_key, priority, true) { return; }
	let released = lifecycle.resolve(key, ResolvedTile::Empty);
	for key in released { requester.release_tile(key.entity, requester_entity, key.tile_key); }
}

pub(crate) fn update_region_loader_requests(
	freeze: Res<FreezeRegionLoader>,
	mut cameras: Query<(Entity, &Camera, &GlobalTransform, &mut RegionLoader), With<Camera3d>>,
	mut streaming: ParamSet<(
		TileRequester,
		Query<(Entity, &GlobalTransform, &EntityStreaming)>,
	)>,
) {
	for (camera_entity, camera, camera_global, tile_class, mut loader) in &mut cameras {
		if !camera.is_active {
			let release: Vec<_> = loader.tiles.entries().map(|(key, _)| key).collect();
			loader.bands.clear();
			loader.classes.clear();
			loader.tiles = Default::default();
			let mut requester = streaming.p0();
			for key in release { requester.release_tile(key.entity, camera_entity, key.tile_key); }
			continue;
		}
		if freeze.0 { continue; }

		let camera_world = camera_global.translation();
		let settings = loader.settings.clone();
		let mut acquisitions = Vec::new();
		let mut releases = Vec::new();
		{
			let entitys = streaming.p1();
			let mut acquire = Vec::new();
			let mut release = Vec::new();
			for (entity_id, entity_global, entity_streaming) in &entitys {
				let camera_local = entity_global.affine().inverse().transform_point3(camera_world);
				let delta = update_desired_sources_delta(
					&mut loader,
					entity_id,
					tile_class.0,
					nearest_chunk_center(camera_local),
					&settings,
					entity_streaming,
				);
				loader.tiles.apply_delta(&delta.added, &delta.removed, &mut acquire, &mut release);
				releases.extend(release.drain(..));
				for key in acquire.drain(..) {
					let center_local = ((key.tile_key.region.min() + key.tile_key.region.size().as_ivec3() / 2) * CHUNK_SIZE as i32).as_vec3();
					let priority = -camera_world.distance(entity_global.transform_point(center_local));
					acquisitions.push((key, priority));
				}
			}
		}

		let mut requester = streaming.p0();
		for key in releases { requester.release_tile(key.entity, camera_entity, key.tile_key); }
		for (key, priority) in acquisitions {
			if !loader.tiles.contains_source(key) { continue; }
			acquire_tile(&mut requester, &mut loader.tiles, camera_entity, key, priority);
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
		let key = EntityTileKey { entity: result.entity, tile_key: result.key };
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
	mut requester_streaming: ParamSet<(
		TileRequester,
		Query<(&EntityStreaming, Option<&TileBuildingParameters>)>,
	)>,
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
						let Ok((entity_streaming)) = entitys.get(event.entity) else { continue };
						changed.clear();
						for_each_tile_in_bands(bands, event.region, |lod, min| {
							let key = EntityTileKey::new(event.entity, class, lod, min);
							if !loader.tiles.contains_desired(key) && tile_has_present_source(entity_streaming, key) { changed.push(key); }
						});
						loader.tiles.apply_delta(&changed, &[], &mut acquire, &mut release);
						releases.extend(release.drain(..));
						acquisitions.extend(acquire.drain(..).map(|key| key));
					}
					ChunkAvailabilityChangeKind::BecameEmpty => {
						let Ok((entity_streaming, _)) = entitys.get(event.entity) else { continue };
						loader.tiles.desired_in_area(event.entity, event.region, &mut changed);
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
