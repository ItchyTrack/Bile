use std::sync::Arc;

use bevy::{
	asset::RenderAssetUsages,
	ecs::component::{ComponentId, ComponentIdFor},
	mesh::{Indices, PrimitiveTopology},
	prelude::*,
	tasks::AsyncComputeTaskPool,
};
use bevy_trait_query::RegisterExt;
use bile_math::NonZeroRegion;
use bile_scene::NodeBounds;
use bile_tiles::{BileTilesSchedule, BileTilesSystemSet, ComponentSource, DesiredComponents, FinishedRequestMessage, RegisterTileClass, TileKey};
use crossbeam_channel::{Receiver, Sender};
use fastnoise_lite::{FastNoiseLite, FractalType, NoiseType};

use crate::MainRenderingLoader;

pub const TILE_SIZE: u32 = 64;
const TILE_QUADS: usize = 32;
const TERRAIN_AMPLITUDE: f32 = 160.0;
const TERRAIN_HALF_WIDTH: i32 = 32768;
const TERRAIN_HALF_HEIGHT: i32 = 256;

#[derive(Component, Default)]
pub struct TerrainClass;

#[derive(Resource)]
struct TerrainMaterials(Vec<Handle<StandardMaterial>>);

impl TerrainMaterials {
	fn for_lod(&self, lod: u8) -> Handle<StandardMaterial> {
		self.0[(lod as usize).min(self.0.len() - 1)].clone()
	}
}

pub fn lod_color(lod: u8) -> Color {
	Color::hsl(100.0 + lod as f32 * 45.0, 0.45, 0.5)
}

struct TerrainMeshResult {
	tile_entity: Entity,
	tile_key: TileKey,
	entity: Entity,
	mesh: Option<Mesh>,
}

#[derive(Resource)]
pub struct TerrainMeshSource {
	noise: Arc<FastNoiseLite>,
	mesh3d: ComponentId,
	sender: Sender<TerrainMeshResult>,
	receiver: Receiver<TerrainMeshResult>,
}

impl TerrainMeshSource {
	fn new(mesh3d: ComponentId) -> Self {
		let mut noise = FastNoiseLite::new();
		noise.set_noise_type(Some(NoiseType::OpenSimplex2));
		noise.set_frequency(Some(0.0015));
		noise.set_fractal_type(Some(FractalType::FBm));
		noise.set_fractal_octaves(Some(5));
		let (sender, receiver) = crossbeam_channel::unbounded();
		Self { noise: Arc::new(noise), mesh3d, sender, receiver }
	}
}

impl ComponentSource for TerrainMeshSource {
	fn component_cost(&self, _entity: Entity, _tile_key: TileKey, component: ComponentId) -> Option<f32> {
		(component == self.mesh3d).then_some(1.0)
	}

	fn request_component(&mut self, entity: Entity, tile_entity: Entity, tile_key: TileKey, component: ComponentId) {
		assert!(component == self.mesh3d);
		let noise = self.noise.clone();
		let sender = self.sender.clone();
		AsyncComputeTaskPool::get().spawn(async move {
			let mesh = build_tile_mesh(&noise, tile_key);
			let _ = sender.send(TerrainMeshResult { tile_entity, tile_key, entity, mesh });
		}).detach();
	}
}

fn terrain_height(noise: &FastNoiseLite, x: f32, z: f32) -> f32 {
	noise.get_noise_2d(x, z) * TERRAIN_AMPLITUDE
}

fn build_tile_mesh(noise: &FastNoiseLite, tile_key: TileKey) -> Option<Mesh> {
	let min = tile_key.region.min();
	let size = tile_key.region.size().x as f32;
	let step = size / TILE_QUADS as f32;
	let samples = TILE_QUADS + 3;
	let mut heights = Vec::with_capacity(samples * samples);
	for j in 0..samples {
		for i in 0..samples {
			let x = min.x as f32 + (i as f32 - 1.0) * step;
			let z = min.z as f32 + (j as f32 - 1.0) * step;
			heights.push(terrain_height(noise, x, z));
		}
	}
	let height = |i: usize, j: usize| heights[j * samples + i];

	let y_min = min.y as f32;
	let y_max = y_min + size;
	let vertex = |i: usize, j: usize| (j * (TILE_QUADS + 1) + i) as u32;
	let mut indices = Vec::new();
	for j in 0..TILE_QUADS {
		for i in 0..TILE_QUADS {
			let average = (height(i + 1, j + 1) + height(i + 2, j + 1) + height(i + 1, j + 2) + height(i + 2, j + 2)) / 4.0;
			if average < y_min || average >= y_max { continue; }
			let (a, b, c, d) = (vertex(i, j), vertex(i + 1, j), vertex(i, j + 1), vertex(i + 1, j + 1));
			indices.extend_from_slice(&[a, c, b, b, c, d]);
		}
	}
	if indices.is_empty() {
		return None;
	}

	let mut positions = Vec::with_capacity((TILE_QUADS + 1) * (TILE_QUADS + 1));
	let mut normals = Vec::with_capacity(positions.capacity());
	for j in 1..=TILE_QUADS + 1 {
		for i in 1..=TILE_QUADS + 1 {
			let x = min.x as f32 + (i as f32 - 1.0) * step;
			let z = min.z as f32 + (j as f32 - 1.0) * step;
			positions.push([x, height(i, j), z]);
			let normal = Vec3::new(height(i - 1, j) - height(i + 1, j), 2.0 * step, height(i, j - 1) - height(i, j + 1)).normalize();
			normals.push(normal.to_array());
		}
	}

	let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
	mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
	mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
	mesh.insert_indices(Indices::U32(indices));
	Some(mesh)
}

fn receive_terrain_meshes(
	mut commands: Commands,
	source: Res<TerrainMeshSource>,
	tiles: Query<&DesiredComponents>,
	materials: Res<TerrainMaterials>,
	mut meshes: ResMut<Assets<Mesh>>,
	mut finished: MessageWriter<FinishedRequestMessage>,
) {
	for result in source.receiver.try_iter() {
		let Ok(desired_components) = tiles.get(result.tile_entity) else { continue };
		if !desired_components.is_component_desired(source.mesh3d) { continue; }
		if let Some(mesh) = result.mesh {
			commands.entity(result.tile_entity).try_insert((
				Mesh3d(meshes.add(mesh)),
				MeshMaterial3d(materials.for_lod(result.tile_key.lod)),
			));
		}
		finished.write(FinishedRequestMessage {
			entity: result.entity,
			tile_entity: result.tile_entity,
			tile_key: result.tile_key,
			component_id: source.mesh3d,
		});
	}
}

fn decide_terrain_components(
	terrain_class: ComponentIdFor<TerrainClass>,
	mesh3d: ComponentIdFor<Mesh3d>,
	mut tiles: Query<&mut DesiredComponents, Added<TerrainClass>>,
) {
	for mut desired_components in &mut tiles {
		desired_components.add_desired_component(terrain_class.get(), mesh3d.get());
	}
}

fn spawn_terrain(
	mut commands: Commands,
	mut materials: ResMut<Assets<StandardMaterial>>,
) {
	let lod_materials = (0..8)
		.map(|lod| materials.add(StandardMaterial { base_color: lod_color(lod), perceptual_roughness: 0.9, ..default() }))
		.collect();
	commands.insert_resource(TerrainMaterials(lod_materials));

	let bounds = NonZeroRegion::from_min_end(
		IVec3::new(-TERRAIN_HALF_WIDTH, -TERRAIN_HALF_HEIGHT, -TERRAIN_HALF_WIDTH),
		IVec3::new(TERRAIN_HALF_WIDTH, TERRAIN_HALF_HEIGHT, TERRAIN_HALF_WIDTH),
	).unwrap();
	commands.spawn((
		Name::new("Terrain"),
		Transform::default(),
		Visibility::default(),
		NodeBounds::new(bounds),
		MainRenderingLoader::new(TILE_SIZE),
	));
}

pub struct TerrainPlugin;

impl Plugin for TerrainPlugin {
	fn build(&self, app: &mut App) {
		app.world_mut().register_component::<TerrainClass>();
		let mesh3d = app.world_mut().register_component::<Mesh3d>();
		app.register_tile_class::<TerrainClass>()
			.register_component_as::<dyn ComponentSource, TerrainMeshSource>()
			.insert_resource(TerrainMeshSource::new(mesh3d))
			.add_systems(Startup, spawn_terrain)
			.add_systems(Update, receive_terrain_meshes)
			.add_systems(BileTilesSchedule, decide_terrain_components.in_set(BileTilesSystemSet::UpdateWantedComponents));
	}
}
