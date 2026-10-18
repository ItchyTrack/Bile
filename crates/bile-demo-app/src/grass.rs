use std::sync::Arc;

use bevy::{
	asset::RenderAssetUsages,
	ecs::component::{ComponentId, ComponentIdFor},
	mesh::{Indices, PrimitiveTopology},
	prelude::*,
	tasks::AsyncComputeTaskPool,
};
use bevy_trait_query::RegisterExt;
use bile_tiles::{BileTilesSchedule, BileTilesSystemSet, ComponentSource, DesiredComponents, FinishedRequestMessage, TileKey};
use crossbeam_channel::{Receiver, Sender};
use fastnoise_lite::{FastNoiseLite, FractalType, NoiseType};

use crate::terrain::TerrainClass;

const TERRAIN_AMPLITUDE: f32 = 160.0;
const BLADES_PER_TILE: u32 = 6000;
const BLADE_HEIGHT: f32 = 0.9;
const BLADE_WIDTH: f32 = 0.12;

#[derive(Component, Default)]
pub struct Grass;

#[derive(Resource)]
struct GrassMaterial(Handle<StandardMaterial>);

struct GrassMeshResult {
	tile_entity: Entity,
	tile_key: TileKey,
	entity: Entity,
	mesh: Option<Mesh>,
}

#[derive(Resource)]
pub struct GrassSource {
	noise: Arc<FastNoiseLite>,
	grass: ComponentId,
	sender: Sender<GrassMeshResult>,
	receiver: Receiver<GrassMeshResult>,
}

impl GrassSource {
	fn new(grass: ComponentId) -> Self {
		let mut noise = FastNoiseLite::new();
		noise.set_noise_type(Some(NoiseType::OpenSimplex2));
		noise.set_frequency(Some(0.0015));
		noise.set_fractal_type(Some(FractalType::FBm));
		noise.set_fractal_octaves(Some(5));
		let (sender, receiver) = crossbeam_channel::unbounded();
		Self { noise: Arc::new(noise), grass, sender, receiver }
	}
}

impl ComponentSource for GrassSource {
	fn component_cost(&self, _entity: Entity, _tile_key: TileKey, component: ComponentId) -> Option<f32> {
		(component == self.grass).then_some(1.0)
	}

	fn request_component(&mut self, entity: Entity, tile_entity: Entity, tile_key: TileKey, component: ComponentId) {
		assert!(component == self.grass);
		let noise = self.noise.clone();
		let sender = self.sender.clone();
		AsyncComputeTaskPool::get().spawn(async move {
			let mesh = build_grass_mesh(&noise, tile_key);
			let _ = sender.send(GrassMeshResult { tile_entity, tile_key, entity, mesh });
		}).detach();
	}
}

fn hash(mut x: u32) -> u32 {
	x ^= x >> 16;
	x = x.wrapping_mul(0x7feb352d);
	x ^= x >> 15;
	x = x.wrapping_mul(0x846ca68b);
	x ^ (x >> 16)
}

fn build_grass_mesh(noise: &FastNoiseLite, tile_key: TileKey) -> Option<Mesh> {
	let min = tile_key.region.min();
	let size = tile_key.region.size().x as f32;
	let seed = hash(min.x as u32 ^ hash(min.z as u32 ^ hash(min.y as u32)));
	let mut state = seed;
	let mut random = || {
		state = hash(state.wrapping_add(0x9e3779b9));
		state as f32 / u32::MAX as f32
	};

	let mut positions = Vec::new();
	let mut normals = Vec::new();
	let mut colors = Vec::new();
	for _ in 0..BLADES_PER_TILE {
		let x = min.x as f32 + random() * size;
		let z = min.z as f32 + random() * size;
		let angle = random() * std::f32::consts::TAU;
		let height = BLADE_HEIGHT * (0.6 + random() * 0.8);
		let shade = 0.7 + random() * 0.3;
		let y = noise.get_noise_2d(x, z) * TERRAIN_AMPLITUDE;
		if y < min.y as f32 || y >= min.y as f32 + size { continue; }

		let side = Vec3::new(angle.cos(), 0.0, angle.sin()) * BLADE_WIDTH;
		let lean = Vec3::new(random() - 0.5, 0.0, random() - 0.5) * 0.4;
		let base = Vec3::new(x, y, z);
		positions.extend_from_slice(&[(base - side).to_array(), (base + side).to_array(), (base + lean + Vec3::Y * height).to_array()]);
		normals.extend_from_slice(&[[0.0, 1.0, 0.0]; 3]);
		colors.extend_from_slice(&[[0.1 * shade, 0.3 * shade, 0.05, 1.0], [0.1 * shade, 0.3 * shade, 0.05, 1.0], [0.35 * shade, 0.75 * shade, 0.15, 1.0]]);
	}
	if positions.is_empty() {
		return None;
	}

	let indices = (0..positions.len() as u32).collect();
	let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
	mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
	mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
	mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
	mesh.insert_indices(Indices::U32(indices));
	Some(mesh)
}

fn decide_grass_components(
	terrain_class: ComponentIdFor<TerrainClass>,
	grass: ComponentIdFor<Grass>,
	mut tiles: Query<(&TileKey, &mut DesiredComponents), Added<TerrainClass>>,
) {
	for (tile_key, mut desired_components) in &mut tiles {
		if tile_key.lod == 0 {
			desired_components.add_desired_component(terrain_class.get(), grass.get());
		}
	}
}

fn receive_grass_meshes(
	mut commands: Commands,
	source: Res<GrassSource>,
	tiles: Query<&DesiredComponents>,
	material: Res<GrassMaterial>,
	mut meshes: ResMut<Assets<Mesh>>,
	mut finished: MessageWriter<FinishedRequestMessage>,
) {
	for result in source.receiver.try_iter() {
		let Ok(desired_components) = tiles.get(result.tile_entity) else { continue };
		if !desired_components.is_component_desired(source.grass) { continue; }
		if let Some(mesh) = result.mesh {
			commands.entity(result.tile_entity).try_insert((Transform::default(), Visibility::default()));
			commands.spawn((
				Mesh3d(meshes.add(mesh)),
				MeshMaterial3d(material.0.clone()),
				ChildOf(result.tile_entity),
			));
		}
		finished.write(FinishedRequestMessage {
			entity: result.entity,
			tile_entity: result.tile_entity,
			tile_key: result.tile_key,
			component_id: source.grass,
		});
	}
}

fn setup_grass_material(mut commands: Commands, mut materials: ResMut<Assets<StandardMaterial>>) {
	let material = materials.add(StandardMaterial { perceptual_roughness: 0.8, double_sided: true, cull_mode: None, ..default() });
	commands.insert_resource(GrassMaterial(material));
}

pub struct GrassPlugin;

impl Plugin for GrassPlugin {
	fn build(&self, app: &mut App) {
		let grass = app.world_mut().register_component::<Grass>();
		app.register_component_as::<dyn ComponentSource, GrassSource>()
			.insert_resource(GrassSource::new(grass))
			.add_systems(Startup, setup_grass_material)
			.add_systems(Update, receive_grass_meshes)
			.add_systems(BileTilesSchedule, decide_grass_components.in_set(BileTilesSystemSet::UpdateWantedComponents));
	}
}
