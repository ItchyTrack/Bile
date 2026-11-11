use std::{collections::{HashMap, HashSet}, marker::PhantomData, sync::Arc};

use bevy::{prelude::*, tasks::{block_on, AsyncComputeTaskPool, Task}};
use bevy_trait_query::RegisterExt;
use bile_data::{BileAddDataType, DataConsumingNode, DataPasser, DataProducingNode, DataRequester, DataType, DirtyState, Request};
use bile_tiles::TileKey;
use fastnoise_lite::{FastNoiseLite, FractalType, NoiseType};

pub const TILE_QUADS: u32 = 32;
const TERRAIN_AMPLITUDE: f32 = 160.0;

pub type MeshTile = Option<(Mesh3d, MeshMaterial3d<StandardMaterial>)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HeightmapParams {
	pub min: IVec2,
	pub samples: UVec2,
	pub step: u32,
}

impl HeightmapParams {
	pub fn for_tile(tile_key: TileKey) -> Self {
		let step = tile_key.size().x / TILE_QUADS;
		Self { min: tile_key.min().xz() - step as i32, samples: UVec2::splat(TILE_QUADS + 3), step }
	}
}

pub struct Heights {
	params: HeightmapParams,
	values: Vec<f32>,
}

impl Heights {
	pub fn at(&self, i: usize, j: usize) -> f32 {
		self.values[j * self.params.samples.x as usize + i]
	}

	pub fn sample(&self, position: Vec2) -> f32 {
		let local = (position - self.params.min.as_vec2()) / self.params.step as f32;
		let max = (self.params.samples - 2).as_vec2();
		let cell = local.floor().clamp(Vec2::ZERO, max);
		let t = local - cell;
		let (i, j) = (cell.x as usize, cell.y as usize);
		let near = self.at(i, j).lerp(self.at(i + 1, j), t.x);
		let far = self.at(i, j + 1).lerp(self.at(i + 1, j + 1), t.x);
		near.lerp(far, t.y)
	}
}

pub struct Heightmap;

impl DataType for Heightmap {
	type Params = HeightmapParams;
	type Data = Arc<Heights>;
}

#[derive(Component)]
struct HeightmapSource {
	noise: Arc<FastNoiseLite>,
	tasks: Vec<(HeightmapParams, Task<Heights>)>,
}

impl DataProducingNode<Heightmap> for HeightmapSource {
	fn cost(&self, _request: &Request<Heightmap>) -> Option<f32> { Some(1.0) }

	fn request(&mut self, request: Request<Heightmap>) {
		let noise = self.noise.clone();
		let params = request.params;
		self.tasks.push((params, AsyncComputeTaskPool::get().spawn(async move {
			let values = (0..params.samples.y)
				.flat_map(|j| (0..params.samples.x).map(move |i| params.min + IVec2::new(i as i32, j as i32) * params.step as i32))
				.map(|position| noise.get_noise_2d(position.x as f32, position.y as f32) * TERRAIN_AMPLITUDE)
				.collect();
			Heights { params, values }
		})));
	}

	fn drop(&mut self, params: &HeightmapParams) {
		self.tasks.retain(|(task_params, _)| task_params != params);
	}
}

fn finish_heightmaps(source: Single<(Entity, &mut HeightmapSource)>, mut passer: DataPasser<Heightmap>) {
	let (entity, mut source) = source.into_inner();
	for (params, task) in source.tasks.extract_if(.., |(_, task)| task.is_finished()) {
		passer.pass_data(entity, &params, DirtyState::UpToDate, Arc::new(block_on(task)));
	}
}

pub trait HeightmapMesh: DataType<Params = TileKey, Data = MeshTile> {
	fn build(tile_key: TileKey, heights: &Heights) -> Option<Mesh>;
}

#[derive(Component)]
struct HeightmapMeshNode<T> {
	materials: Vec<Handle<StandardMaterial>>,
	tiles: HashMap<HeightmapParams, HashSet<TileKey>>,
	requested: Vec<TileKey>,
	waiting: HashSet<TileKey>,
	unused: Vec<HeightmapParams>,
	ready: Vec<(TileKey, Arc<Heights>)>,
	building: Vec<(TileKey, Task<Option<Mesh>>)>,
	marker: PhantomData<fn() -> T>,
}

impl<T: HeightmapMesh> DataProducingNode<T> for HeightmapMeshNode<T> {
	fn cost(&self, _request: &Request<T>) -> Option<f32> { Some(1.0) }

	fn request(&mut self, request: Request<T>) {
		self.tiles.entry(HeightmapParams::for_tile(request.params)).or_default().insert(request.params);
		self.requested.push(request.params);
	}

	fn drop(&mut self, tile_key: &TileKey) {
		let params = HeightmapParams::for_tile(*tile_key);
		if let Some(tiles) = self.tiles.get_mut(&params) && tiles.remove(tile_key) && tiles.is_empty() {
			self.tiles.remove(&params);
			self.unused.push(params);
		}
		self.waiting.remove(tile_key);
		self.ready.retain(|(key, _)| key != tile_key);
		self.building.retain(|(key, _)| key != tile_key);
	}
}

impl<T: HeightmapMesh> DataConsumingNode<Heightmap> for HeightmapMeshNode<T> {
	fn pass_data(&mut self, params: &HeightmapParams, _tick: u64, heights: Arc<Heights>) {
		let Some(tiles) = self.tiles.get(params) else { return };
		for &tile_key in tiles {
			if self.waiting.remove(&tile_key) {
				self.ready.push((tile_key, heights.clone()));
			}
		}
	}

	fn dirty_data(&mut self, _params: &HeightmapParams, _last_clean_tick: u64) {}
}

fn update_heightmap_meshes<T: HeightmapMesh>(
	node: Single<(Entity, &mut HeightmapMeshNode<T>)>,
	mut heightmaps: DataRequester<Heightmap>,
	mut passer: DataPasser<T>,
	mut meshes: ResMut<Assets<Mesh>>,
) {
	let (entity, mut node) = node.into_inner();
	let node = &mut *node;
	for tile_key in std::mem::take(&mut node.requested) {
		let params = HeightmapParams::for_tile(tile_key);
		if !node.tiles.get(&params).is_some_and(|tiles| tiles.contains(&tile_key)) { continue; }
		match heightmaps.request(Request::new(params, DirtyState::UpToDate), entity) {
			Some(heights) => node.ready.push((tile_key, heights)),
			None => { node.waiting.insert(tile_key); }
		}
	}
	for params in std::mem::take(&mut node.unused) {
		if !node.tiles.contains_key(&params) {
			heightmaps.cancel(&params, entity);
		}
	}
	for (tile_key, heights) in node.ready.drain(..) {
		node.building.push((tile_key, AsyncComputeTaskPool::get().spawn(async move { T::build(tile_key, &heights) })));
	}
	for (tile_key, task) in node.building.extract_if(.., |(_, task)| task.is_finished()) {
		let material = &node.materials[(tile_key.lod as usize).min(node.materials.len() - 1)];
		let tile = block_on(task).map(|mesh| (Mesh3d(meshes.add(mesh)), MeshMaterial3d(material.clone())));
		passer.pass_data(entity, &tile_key, DirtyState::UpToDate, tile);
	}
}

pub trait AddHeightmapMesh {
	fn add_heightmap_mesh<T: HeightmapMesh>(&mut self, materials: Vec<Handle<StandardMaterial>>) -> &mut Self;
}

impl AddHeightmapMesh for App {
	fn add_heightmap_mesh<T: HeightmapMesh>(&mut self, materials: Vec<Handle<StandardMaterial>>) -> &mut Self {
		let node = HeightmapMeshNode::<T> {
			materials,
			tiles: default(),
			requested: default(),
			waiting: default(),
			unused: default(),
			ready: default(),
			building: default(),
			marker: PhantomData,
		};
		self.world_mut().spawn(node);
		self.bile_add_data_type::<T>()
			.register_component_as::<dyn DataProducingNode<T>, HeightmapMeshNode<T>>()
			.register_component_as::<dyn DataConsumingNode<Heightmap>, HeightmapMeshNode<T>>()
			.add_systems(Update, update_heightmap_meshes::<T>)
	}
}

pub struct HeightmapPlugin;

impl Plugin for HeightmapPlugin {
	fn build(&self, app: &mut App) {
		let mut noise = FastNoiseLite::new();
		noise.set_noise_type(Some(NoiseType::OpenSimplex2));
		noise.set_frequency(Some(0.0015));
		noise.set_fractal_type(Some(FractalType::FBm));
		noise.set_fractal_octaves(Some(5));
		app.world_mut().spawn(HeightmapSource { noise: Arc::new(noise), tasks: Vec::new() });
		app.bile_add_data_type::<Heightmap>()
			.register_component_as::<dyn DataProducingNode<Heightmap>, HeightmapSource>()
			.add_systems(Update, finish_heightmaps);
	}
}
