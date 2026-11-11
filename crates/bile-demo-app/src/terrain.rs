use bevy::{
	asset::RenderAssetUsages,
	mesh::{Indices, PrimitiveTopology},
	prelude::*,
};
use bile_data::DataType;
use bile_math::NonZeroRegion;
use bile_region_loader::{RegionLoaderComponent, RegionLoaderPlugin};
use bile_scene::NodeBounds;
use bile_tiles::TileKey;

use crate::{grass::GrassLoader, heightmap::{AddHeightmapMesh, HeightmapMesh, Heights, MeshTile, TILE_QUADS}};

pub const TILE_SIZE: u32 = 64;
const TILE_QUADS_USIZE: usize = TILE_QUADS as usize;
const TERRAIN_HALF_WIDTH: i32 = 32768;
const TERRAIN_HALF_HEIGHT: i32 = 256;

pub struct TerrainMesh;

impl DataType for TerrainMesh {
	type Params = TileKey;
	type Data = MeshTile;
}

pub type TerrainLoader = RegionLoaderComponent<TerrainMesh>;

pub fn lod_color(lod: u8) -> Color {
	Color::hsl(100.0 + lod as f32 * 45.0, 0.45, 0.5)
}

impl HeightmapMesh for TerrainMesh {
	fn build(tile_key: TileKey, heights: &Heights) -> Option<Mesh> {
		let min = tile_key.region.min();
		let size = tile_key.region.size().x as f32;
		let step = size / TILE_QUADS as f32;
		let height = |i: usize, j: usize| heights.at(i, j);

		let y_min = min.y as f32;
		let y_max = y_min + size;
		let vertex = |i: usize, j: usize| (j * (TILE_QUADS_USIZE + 1) + i) as u32;
		let mut indices = Vec::new();
		for j in 0..TILE_QUADS_USIZE {
			for i in 0..TILE_QUADS_USIZE {
				let average = (height(i + 1, j + 1) + height(i + 2, j + 1) + height(i + 1, j + 2) + height(i + 2, j + 2)) / 4.0;
				if average < y_min || average >= y_max { continue; }
				let (a, b, c, d) = (vertex(i, j), vertex(i + 1, j), vertex(i, j + 1), vertex(i + 1, j + 1));
				indices.extend_from_slice(&[a, c, b, b, c, d]);
			}
		}
		if indices.is_empty() {
			return None;
		}

		let mut positions = Vec::with_capacity((TILE_QUADS_USIZE + 1) * (TILE_QUADS_USIZE + 1));
		let mut normals = Vec::with_capacity(positions.capacity());
		for j in 1..=TILE_QUADS_USIZE + 1 {
			for i in 1..=TILE_QUADS_USIZE + 1 {
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
}

fn spawn_terrain(mut commands: Commands) {
	let bounds = NonZeroRegion::from_min_end(
		IVec3::new(-TERRAIN_HALF_WIDTH, -TERRAIN_HALF_HEIGHT, -TERRAIN_HALF_WIDTH),
		IVec3::new(TERRAIN_HALF_WIDTH, TERRAIN_HALF_HEIGHT, TERRAIN_HALF_WIDTH),
	).unwrap();
	commands.spawn((
		Name::new("Terrain"),
		Transform::default(),
		Visibility::default(),
		NodeBounds::new(bounds),
		TerrainLoader::new(TILE_SIZE),
		GrassLoader::new(TILE_SIZE),
	));
}

pub struct TerrainPlugin;

impl Plugin for TerrainPlugin {
	fn build(&self, app: &mut App) {
		let mut materials = app.world_mut().resource_mut::<Assets<StandardMaterial>>();
		let lod_materials = (0..8)
			.map(|lod| materials.add(StandardMaterial { base_color: lod_color(lod), perceptual_roughness: 0.9, ..default() }))
			.collect();
		app.add_heightmap_mesh::<TerrainMesh>(lod_materials)
			.add_plugins(RegionLoaderPlugin::<TerrainMesh>::default())
			.add_systems(Startup, spawn_terrain);
	}
}
