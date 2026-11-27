use std::{collections::{BTreeSet, HashMap}, sync::Arc};

use bevy::{prelude::*, tasks::{block_on, AsyncComputeTaskPool, Task}};
use bile_data::{DataType, DirtyState, Request, btree_drain, cancel_range::in_cancel_range, simple::{self, diff::BileRegisterDiff, producing::BileRegisterSimpleProducer, storage::BileAddSimpleStorage}};
use bile_tiles::TileKey;
use fastnoise_lite::{FastNoiseLite, FractalType, NoiseType};

pub const HEIGHTMAP_QUADS: usize = 32;
const SAMPLES: usize = HEIGHTMAP_QUADS + 3;
const TERRAIN_AMPLITUDE: f32 = 160.0;
const OVERRIDE_STEP: f32 = 2.0;

fn offset(overrides: &HashMap<IVec2, f32>, position: Vec2) -> f32 {
	let local = position / OVERRIDE_STEP;
	let cell = local.floor();
	let t = local - cell;
	let cell = cell.as_ivec2();
	let at = |x: i32, y: i32| overrides.get(&(cell + IVec2::new(x, y))).copied().unwrap_or(0.0);
	at(0, 0).lerp(at(1, 0), t.x).lerp(at(0, 1).lerp(at(1, 1), t.x), t.y)
}

#[derive(Clone)]
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
		offset(&self.overrides, position)
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
	raised: HashMap<IVec2, f32>, // what the edits added to the overrides
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
				*self.raised.entry(cell).or_default() += amount * falloff * falloff;
			}
		}
		self.edited.push(Rect::from_center_half_size(center, Vec2::splat(radius + OVERRIDE_STEP)));
	}
}

pub type Raised = Arc<HashMap<IVec2, f32>>; // what an edit added to the overrides

pub struct Heightmap;

impl DataType for Heightmap {
	type Params = TileKey;
	type Data = Arc<Heights>;
	type Diff = Raised;
}

#[derive(Component)]
struct HeightmapSource {
	sampler: HeightSampler,
	live: HashMap<TileKey, (BTreeSet<DirtyState>, Option<DirtyState>)>, // the requests it still works on, and the freshest request its last heights were for
	tasks: Vec<(TileKey, Task<Heights>, DirtyState)>, // and the state of the heights, older if the tile was edited since
}

impl HeightmapSource {
	fn spawn(&mut self, tile_key: TileKey) {
		let sampler = self.sampler.clone();
		self.tasks.push((tile_key, AsyncComputeTaskPool::get().spawn(async move { sampler.heights(tile_key) }), DirtyState::UpToDate));
	}
}

impl simple::producing::ProducingNodeAPI<Heightmap> for HeightmapSource {
	fn cost(&self, _request: &Request<Heightmap>) -> Option<f32> { Some(1.0) }

	fn request(&mut self, request: Request<Heightmap>) {
		self.live.entry(request.params).or_default().0.insert(request.allowed_tick);
		// A running task is not restarted, if it is too old for the request another one starts when it is done
		if !self.tasks.iter().any(|(task_tile_key, ..)| *task_tile_key == request.params) {
			self.spawn(request.params);
		}
	}

	fn cancel(&mut self, tile_key: &TileKey, latest: DirtyState, oldest: Option<u64>) {
		let Some((waiting, used)) = self.live.get_mut(tile_key) else { return; };
		waiting.retain(|&dirty_state| !in_cancel_range(dirty_state, latest, oldest));
		if used.is_some_and(|used| in_cancel_range(used, latest, oldest)) { *used = None; }
		if waiting.is_empty() && used.is_none() {
			self.live.remove(tile_key);
			self.tasks.retain(|(task_tile_key, ..)| task_tile_key != tile_key);
		}
	}
}

impl simple::diff::DiffApplyingNode<Heightmap> for HeightmapSource {
	fn diff_cost(&self, _request: &Request<Heightmap>, _diffs: &[Raised]) -> Option<f32> { Some(0.5) }

	fn apply(&mut self, _tile_key: &TileKey, heights: &mut Arc<Heights>, diffs: Vec<Raised>) -> bool {
		let heights = Arc::make_mut(heights);
		for (index, value) in heights.values.iter_mut().enumerate() {
			let position = heights.min + Vec2::new((index % SAMPLES) as f32, (index / SAMPLES) as f32) * heights.step;
			*value += diffs.iter().map(|raised| offset(raised, position)).sum::<f32>();
		}
		true
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
	mut dirtier: simple::producing::DataDirtier<Heightmap>,
) {
	if field.edited.is_empty() { return; }
	let edited = std::mem::take(&mut field.edited);
	let raised = Arc::new(std::mem::take(&mut field.raised));
	let mut source = source.into_inner();
	source.sampler = field.sampler.clone();
	let touched = |tile_key: &TileKey| edited.iter().any(|rect| !footprint(*tile_key).intersect(*rect).is_empty());
	let tick = dirtier.current_tick();

	for (_, _, dirty_state) in source.tasks.iter_mut().filter(|(tile_key, ..)| touched(tile_key)) {
		*dirty_state = (*dirty_state).min(DirtyState::Dirty { tick });
	}
	for tile_key in source.live.keys().filter(|tile_key| touched(tile_key)) {
		dirtier.mark_dirty(tile_key, tick, Some(&raised));
	}
}

fn finish_heightmaps(source: Single<(Entity, &mut HeightmapSource)>, mut passer: simple::producing::DataPasser<Heightmap>) {
	let (entity, mut source) = source.into_inner();
	let finished: Vec<_> = source.tasks.extract_if(.., |(_, task, _)| task.is_finished()).collect();
	for (tile_key, task, dirty_state) in finished {
		passer.pass_data(entity, &tile_key, dirty_state, Arc::new(block_on(task)));
		let Some((waiting, used)) = source.live.get_mut(&tile_key) else { continue; };
		if let Some(&wanted) = btree_drain::set_drain_range(waiting, ..=dirty_state).last() { *used = Some(wanted); }
		if !waiting.is_empty() {
			source.spawn(tile_key);
		}
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
		app.world_mut().spawn(HeightmapSource { sampler: sampler.clone(), live: HashMap::new(), tasks: Vec::new() });
		app.insert_resource(HeightField { sampler, edited: Vec::new(), raised: HashMap::new() });
		app.add_simple_storage::<Heightmap>()
			.register_producer_simple::<Heightmap, HeightmapSource>()
			.register_diff_applier::<Heightmap, HeightmapSource>()
			.add_systems(Update, (apply_height_edits, finish_heightmaps).chain().in_set(HeightmapSystems));
	}
}
