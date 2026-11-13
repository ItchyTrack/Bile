use std::{collections::{HashMap, HashSet}, sync::Arc};

use bevy::{prelude::*, tasks::{block_on, AsyncComputeTaskPool, Task}};
use bile_data::{DataType, DirtyState, Request, simple::{self, BileRegisterSimple}};
use bile_tiles::TileKey;
use fastnoise_lite::{FastNoiseLite, FractalType, NoiseType};

pub const HEIGHTMAP_QUADS: usize = 32;
const SAMPLES: usize = HEIGHTMAP_QUADS + 3;
const TERRAIN_AMPLITUDE: f32 = 160.0;
const OVERRIDE_STEP: f32 = 2.0;

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

#[derive(Clone)]
struct HeightSampler {
	noise: Arc<FastNoiseLite>,
	overrides: Arc<HashMap<IVec2, f32>>,
}

impl HeightSampler {
	fn height(&self, position: Vec2) -> f32 {
		self.noise.get_noise_2d(position.x, position.y) * TERRAIN_AMPLITUDE + self.offset(position)
	}

	fn offset(&self, position: Vec2) -> f32 {
		if self.overrides.is_empty() { return 0.0; }
		let local = position / OVERRIDE_STEP;
		let cell = local.floor();
		let t = local - cell;
		let cell = cell.as_ivec2();
		let at = |x: i32, y: i32| self.overrides.get(&(cell + IVec2::new(x, y))).copied().unwrap_or(0.0);
		at(0, 0).lerp(at(1, 0), t.x).lerp(at(0, 1).lerp(at(1, 1), t.x), t.y)
	}

	fn heights(&self, tile_key: TileKey) -> Heights {
		let step = tile_key.size().x as f32 / HEIGHTMAP_QUADS as f32;
		let min = tile_key.min().xz().as_vec2() - step;
		let values = (0..SAMPLES)
			.flat_map(|j| (0..SAMPLES).map(move |i| min + Vec2::new(i as f32, j as f32) * step))
			.map(|position| self.height(position))
			.collect();
		Heights { min, step, values }
	}
}

#[derive(Resource)]
pub struct HeightField {
	sampler: HeightSampler,
	edited: Vec<Rect>,
}

impl HeightField {
	pub fn height(&self, position: Vec2) -> f32 {
		self.sampler.height(position)
	}

	pub fn raise(&mut self, center: Vec2, radius: f32, amount: f32) {
		let overrides = Arc::make_mut(&mut self.sampler.overrides);
		let min = ((center - radius) / OVERRIDE_STEP).floor().as_ivec2();
		let max = ((center + radius) / OVERRIDE_STEP).ceil().as_ivec2();
		for y in min.y..=max.y {
			for x in min.x..=max.x {
				let cell = IVec2::new(x, y);
				let distance = (cell.as_vec2() * OVERRIDE_STEP).distance(center) / radius;
				if distance >= 1.0 { continue; }
				let falloff = 1.0 - distance * distance;
				*overrides.entry(cell).or_default() += amount * falloff * falloff;
			}
		}
		self.edited.push(Rect::from_center_half_size(center, Vec2::splat(radius + OVERRIDE_STEP)));
	}
}

pub struct Heightmap;

impl DataType for Heightmap {
	type Params = TileKey;
	type Data = Arc<Heights>;
	type Diff = ();
}

#[derive(Component)]
struct HeightmapSource {
	sampler: HeightSampler,
	live: HashSet<TileKey>,
	tasks: Vec<(TileKey, Task<Heights>)>,
}

impl HeightmapSource {
	fn spawn(&mut self, tile_key: TileKey) {
		let sampler = self.sampler.clone();
		self.tasks.push((tile_key, AsyncComputeTaskPool::get().spawn(async move { sampler.heights(tile_key) })));
	}
}

impl simple::DataProducingNode<Heightmap> for HeightmapSource {
	fn cost(&self, _request: &Request<Heightmap>) -> Option<f32> { Some(1.0) }

	fn request(&mut self, request: Request<Heightmap>) {
		self.live.insert(request.params);
		self.tasks.retain(|(task_tile_key, _)| *task_tile_key != request.params);
		self.spawn(request.params);
	}

	fn drop(&mut self, tile_key: &TileKey) {
		self.live.remove(tile_key);
		self.tasks.retain(|(task_tile_key, _)| task_tile_key != tile_key);
	}
}

fn footprint(tile_key: TileKey) -> Rect {
	let step = tile_key.size().x as f32 / HEIGHTMAP_QUADS as f32;
	let min = tile_key.min().xz().as_vec2() - step;
	Rect::from_corners(min, min + (SAMPLES - 1) as f32 * step)
}

fn apply_height_edits(
	mut field: ResMut<HeightField>,
	source: Single<&mut HeightmapSource>,
	mut dirtier: simple::DataDirtier<Heightmap>,
) {
	if field.edited.is_empty() { return; }
	let edited = std::mem::take(&mut field.edited);
	let mut source = source.into_inner();
	source.sampler = field.sampler.clone();
	let touched = |tile_key: &TileKey| edited.iter().any(|rect| !footprint(*tile_key).intersect(*rect).is_empty());

	let restart: Vec<_> = source.tasks.extract_if(.., |(tile_key, _)| touched(tile_key)).map(|(tile_key, _)| tile_key).collect();
	for tile_key in restart {
		source.spawn(tile_key);
	}
	for tile_key in source.live.iter().filter(|tile_key| touched(tile_key)) {
		dirtier.mark_dirty(tile_key, None);
	}
}

fn finish_heightmaps(source: Single<(Entity, &mut HeightmapSource)>, mut passer: simple::DataPasser<Heightmap>) {
	let (entity, mut source) = source.into_inner();
	for (tile_key, task) in source.tasks.extract_if(.., |(_, task)| task.is_finished()) {
		passer.pass_data(entity, &tile_key, DirtyState::UpToDate, Arc::new(block_on(task)));
	}
}

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HeightmapSystems;

pub struct HeightmapPlugin;

impl Plugin for HeightmapPlugin {
	fn build(&self, app: &mut App) {
		let mut noise = FastNoiseLite::new();
		noise.set_noise_type(Some(NoiseType::OpenSimplex2));
		noise.set_frequency(Some(0.0015));
		noise.set_fractal_type(Some(FractalType::FBm));
		noise.set_fractal_octaves(Some(5));
		let sampler = HeightSampler { noise: Arc::new(noise), overrides: Arc::default() };
		app.world_mut().spawn(HeightmapSource { sampler: sampler.clone(), live: HashSet::new(), tasks: Vec::new() });
		app.insert_resource(HeightField { sampler, edited: Vec::new() });
		app.register_producer_simple::<Heightmap, HeightmapSource>()
			.add_systems(Update, (apply_height_edits, finish_heightmaps).chain().in_set(HeightmapSystems));
	}
}
