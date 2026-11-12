use std::sync::Arc;

use bevy::{prelude::*, tasks::{block_on, AsyncComputeTaskPool, Task}};
use bevy_trait_query::RegisterExt;
use bile_data::{BileAddDataType, DataPasser, DataProducingNode, DataType, DirtyState, Request};
use bile_tiles::TileKey;
use fastnoise_lite::{FastNoiseLite, FractalType, NoiseType};

pub const HEIGHTMAP_QUADS: usize = 32;
const SAMPLES: usize = HEIGHTMAP_QUADS + 3;
const TERRAIN_AMPLITUDE: f32 = 160.0;

pub struct Heights {
	min: Vec2,
	step: f32,
	values: Vec<f32>,
}

impl Heights {
	pub fn at(&self, i: usize, j: usize) -> f32 {
		self.values[j * SAMPLES + i]
	}

	pub fn sample(&self, position: Vec2) -> f32 {
		let local = (position - self.min) / self.step;
		let max = Vec2::splat((SAMPLES - 2) as f32);
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
	type Params = TileKey;
	type Data = Arc<Heights>;
}

#[derive(Component)]
struct HeightmapSource {
	noise: Arc<FastNoiseLite>,
	tasks: Vec<(TileKey, Task<Heights>)>,
}

impl DataProducingNode<Heightmap> for HeightmapSource {
	fn cost(&self, _request: &Request<Heightmap>) -> Option<f32> { Some(1.0) }

	fn request(&mut self, request: Request<Heightmap>) {
		let noise = self.noise.clone();
		let tile_key = request.params;
		self.tasks.push((tile_key, AsyncComputeTaskPool::get().spawn(async move {
			let step = tile_key.size().x as f32 / HEIGHTMAP_QUADS as f32;
			let min = tile_key.min().xz().as_vec2() - step;
			let values = (0..SAMPLES)
				.flat_map(|j| (0..SAMPLES).map(move |i| min + Vec2::new(i as f32, j as f32) * step))
				.map(|position| noise.get_noise_2d(position.x, position.y) * TERRAIN_AMPLITUDE)
				.collect();
			Heights { min, step, values }
		})));
	}

	fn drop(&mut self, tile_key: &TileKey) {
		self.tasks.retain(|(task_tile_key, _)| task_tile_key != tile_key);
	}
}

fn finish_heightmaps(source: Single<(Entity, &mut HeightmapSource)>, mut passer: DataPasser<Heightmap>) {
	let (entity, mut source) = source.into_inner();
	for (tile_key, task) in source.tasks.extract_if(.., |(_, task)| task.is_finished()) {
		passer.pass_data(entity, &tile_key, DirtyState::UpToDate, Arc::new(block_on(task)));
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
		app.bile_add_data_type::<Heightmap>();
		app.register_component_as::<dyn DataProducingNode<Heightmap>, HeightmapSource>()
			.add_systems(Update, finish_heightmaps);
	}
}
