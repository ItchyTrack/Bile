use std::sync::Arc;

use bevy::{prelude::*, tasks::{block_on, AsyncComputeTaskPool, Task}};
use bevy_trait_query::RegisterExt;
use bile_data::{BileAddDataType, DataPasser, DataProducingNode, DataType, DirtyState, Request};
use fastnoise_lite::{FastNoiseLite, FractalType, NoiseType};

const TERRAIN_AMPLITUDE: f32 = 160.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HeightmapParams {
	pub min: IVec2,
	pub samples: UVec2,
	pub step: u32,
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
