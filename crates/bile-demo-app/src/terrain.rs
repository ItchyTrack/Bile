use bevy::{
	asset::RenderAssetUsages,
	ecs::component::ComponentIdFor,
	mesh::{Indices, PrimitiveTopology},
	prelude::*,
};
use bile_components::{AddComponentData, ComponentData};
use bile_data::DataType;
use bile_math::NonZeroRegion;
use bile_scene::NodeBounds;
use bile_tiles::{BileTilesSchedule, BileTilesSystemSet, DesireForClass, RegisterTileClass, TileKey};

use crate::{MainRenderingLoader, heightmap::{AddHeightmapMesh, HeightmapMesh, Heights}};

pub const TILE_SIZE: u32 = 64;
const TILE_QUADS: usize = crate::heightmap::TILE_QUADS as usize;
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

pub struct TerrainMesh;

impl DataType for TerrainMesh {
	type Params = TileKey;
	type Data = Option<Mesh3d>;
}

impl ComponentData for TerrainMesh {
	type Component = Mesh3d;
}

impl HeightmapMesh for TerrainMesh {
	fn build(tile_key: TileKey, heights: &Heights) -> Option<Mesh> {
		build_tile_mesh(heights, tile_key)
	}
	fn component(mesh: Handle<Mesh>) -> Mesh3d {
		Mesh3d(mesh)
	}
}

fn build_tile_mesh(heights: &Heights, tile_key: TileKey) -> Option<Mesh> {
	let min = tile_key.region.min();
	let size = tile_key.region.size().x as f32;
	let step = size / TILE_QUADS as f32;
	let height = |i: usize, j: usize| heights.at(i, j);

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

fn add_terrain_materials(
	mut commands: Commands,
	materials: Res<TerrainMaterials>,
	tiles: Query<(Entity, &TileKey), (With<TerrainClass>, Added<Mesh3d>)>,
) {
	for (entity, tile_key) in &tiles {
		commands.entity(entity).insert(MeshMaterial3d(materials.for_lod(tile_key.lod)));
	}
}

fn decide_terrain_components(
	mut commands: Commands,
	terrain_class: ComponentIdFor<TerrainClass>,
	tiles: Query<(Entity, &TileKey), Added<TerrainClass>>,
) {
	for (entity, tile_key) in &tiles {
		commands.entity(entity).add_desired_component::<TerrainMesh>(terrain_class.get(), *tile_key);
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
		app.register_tile_class::<TerrainClass>()
			.add_component_data::<TerrainMesh>()
			.add_heightmap_mesh::<TerrainMesh>()
			.add_systems(Startup, spawn_terrain)
			.add_systems(PostUpdate, add_terrain_materials)
			.add_systems(BileTilesSchedule, decide_terrain_components.in_set(BileTilesSystemSet::UpdateWantedComponents));
	}
}
