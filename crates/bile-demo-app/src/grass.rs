use bevy::{
	asset::RenderAssetUsages,
	ecs::component::ComponentIdFor,
	mesh::{Indices, PrimitiveTopology},
	prelude::*,
};
use bile_components::{AddComponentData, ComponentData};
use bile_data::DataType;
use bile_tiles::{BileTilesSchedule, BileTilesSystemSet, DesireForClass, TileKey};

use crate::{heightmap::{AddHeightmapMesh, HeightmapMesh, Heights}, terrain::TerrainClass};

const BLADES_PER_TILE: u32 = 6000;
const BLADE_HEIGHT: f32 = 0.9;
const BLADE_WIDTH: f32 = 0.12;

#[derive(Component, Clone)]
pub struct Grass(Handle<Mesh>);

#[derive(Component)]
struct GrassBlades(Entity);

#[derive(Resource)]
struct GrassMaterial(Handle<StandardMaterial>);

pub struct GrassMesh;

impl DataType for GrassMesh {
	type Params = TileKey;
	type Data = Option<Grass>;
}

impl ComponentData for GrassMesh {
	type Component = Grass;
}

impl HeightmapMesh for GrassMesh {
	fn build(tile_key: TileKey, heights: &Heights) -> Option<Mesh> {
		build_grass_mesh(heights, tile_key)
	}
	fn component(mesh: Handle<Mesh>) -> Grass {
		Grass(mesh)
	}
}

fn hash(mut x: u32) -> u32 {
	x ^= x >> 16;
	x = x.wrapping_mul(0x7feb352d);
	x ^= x >> 15;
	x = x.wrapping_mul(0x846ca68b);
	x ^ (x >> 16)
}

fn build_grass_mesh(heights: &Heights, tile_key: TileKey) -> Option<Mesh> {
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
		let y = heights.sample(Vec2::new(x, z));
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
	mut commands: Commands,
	terrain_class: ComponentIdFor<TerrainClass>,
	tiles: Query<(Entity, &TileKey), Added<TerrainClass>>,
) {
	for (entity, tile_key) in &tiles {
		if tile_key.lod == 0 {
			commands.entity(entity).add_desired_component::<GrassMesh>(terrain_class.get(), *tile_key);
		}
	}
}

fn spawn_grass_blades(
	mut commands: Commands,
	material: Res<GrassMaterial>,
	tiles: Query<(Entity, &Grass, Option<&GrassBlades>), Changed<Grass>>,
) {
	for (entity, grass, blades) in &tiles {
		if let Some(blades) = blades {
			commands.entity(blades.0).try_despawn();
		}
		let blades = commands.spawn((Mesh3d(grass.0.clone()), MeshMaterial3d(material.0.clone()), ChildOf(entity))).id();
		commands.entity(entity).insert((GrassBlades(blades), Transform::default(), Visibility::default()));
	}
}

fn despawn_grass_blades(remove: On<Remove, Grass>, mut commands: Commands, tiles: Query<&GrassBlades>) {
	if let Ok(blades) = tiles.get(remove.entity) {
		commands.entity(blades.0).try_despawn();
		commands.entity(remove.entity).try_remove::<GrassBlades>();
	}
}

fn setup_grass_material(mut commands: Commands, mut materials: ResMut<Assets<StandardMaterial>>) {
	let material = materials.add(StandardMaterial { perceptual_roughness: 0.8, double_sided: true, cull_mode: None, ..default() });
	commands.insert_resource(GrassMaterial(material));
}

pub struct GrassPlugin;

impl Plugin for GrassPlugin {
	fn build(&self, app: &mut App) {
		app.add_component_data::<GrassMesh>()
			.add_heightmap_mesh::<GrassMesh>()
			.add_systems(Startup, setup_grass_material)
			.add_systems(PostUpdate, spawn_grass_blades)
			.add_observer(despawn_grass_blades)
			.add_systems(BileTilesSchedule, decide_grass_components.in_set(BileTilesSystemSet::UpdateWantedComponents));
	}
}
