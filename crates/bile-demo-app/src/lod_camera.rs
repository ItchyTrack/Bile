//! Sets the regions of every [`MainRenderingLoader`] to lod bands around the [`LodCamera`], the
//! same way VGE's camera voxel loader does, and draws them.

use bevy::{ecs::component::{ComponentId, ComponentIdFor}, prelude::*};
use bile_math::Region;
use bile_region_loader::LodBand;
use bile_scene::NodeBounds;

use crate::{MainRenderingLoader, terrain::{TerrainClass, lod_color}};

#[derive(Resource, Debug, Clone)]
pub struct LodCameraSettings {
	pub max_lod: u8,
	/// Radius of the lod 0 box in lod 0 tiles.
	pub near_radius_tiles: u32,
	/// How many tiles of each lod surround the previous lod.
	pub rings_per_lod: u32,
	pub frozen: bool,
	pub show_regions: bool,
	pub show_pending_tiles: bool,
}

impl Default for LodCameraSettings {
	fn default() -> Self {
		Self { max_lod: 6, near_radius_tiles: 3, rings_per_lod: 3, frozen: false, show_regions: true, show_pending_tiles: false }
	}
}

/// The camera loaders build their bands around.
#[derive(Component, Default, Clone)]
pub struct LodCamera;

#[derive(Component)]
struct LodCameraText;

/// Concentric bands around `position`. Lod 0 covers a box of `near_radius_tiles` around the
/// nearest tile corner and each further lod adds `rings_per_lod` of its own tiles around the last.
/// Every band but the last is aligned to the next lod's tiles so the next band's inner box is too.
pub fn lod_bands_around(position: Vec3, tile_size: u32, class: ComponentId, settings: &LodCameraSettings) -> Vec<LodBand> {
	// Positions here are in lod 0 tiles.
	let center = (position / tile_size as f32).round().as_ivec3();
	let radius = settings.near_radius_tiles as i32;
	let to_region = |(min, max): (IVec3, IVec3)| Region::from_min_end(min * tile_size as i32, max * tile_size as i32).unwrap();

	let near = align_bounds(center - radius, center + radius + 1, 2);
	let mut bands = vec![LodBand { class, lod: 0, outer: to_region(near), inner: None }];
	let mut previous = near;
	for lod in 1..=settings.max_lod {
		let expand = IVec3::splat((1 << lod) * settings.rings_per_lod as i32);
		let mut outer = (previous.0 - expand, previous.1 + expand);
		if lod < settings.max_lod {
			outer = align_bounds(outer.0, outer.1, 1 << (lod + 1));
		}
		bands.push(LodBand { class, lod, outer: to_region(outer), inner: Some(to_region(previous)) });
		previous = outer;
	}
	bands
}

fn align_bounds(min: IVec3, max: IVec3, size: i32) -> (IVec3, IVec3) {
	(min.div_euclid(IVec3::splat(size)) * size, (max + (size - 1)).div_euclid(IVec3::splat(size)) * size)
}

fn set_loader_regions(
	settings: Res<LodCameraSettings>,
	terrain_class: ComponentIdFor<TerrainClass>,
	cameras: Query<&GlobalTransform, With<LodCamera>>,
	mut loaders: Query<(&GlobalTransform, &mut MainRenderingLoader)>,
) {
	if settings.frozen { return; }
	let Ok(camera) = cameras.single() else { return };
	for (loader_transform, mut loader) in &mut loaders {
		let local = loader_transform.affine().inverse().transform_point3(camera.translation());
		let bands = lod_bands_around(local, loader.tile_size(), terrain_class.get(), &settings);
		loader.set_regions(bands);
	}
}

fn handle_input(keys: Res<ButtonInput<KeyCode>>, mut settings: ResMut<LodCameraSettings>) {
	if keys.just_pressed(KeyCode::KeyF) { settings.frozen = !settings.frozen; }
	if keys.just_pressed(KeyCode::KeyG) { settings.show_regions = !settings.show_regions; }
	if keys.just_pressed(KeyCode::KeyT) { settings.show_pending_tiles = !settings.show_pending_tiles; }
}

fn draw_regions(
	settings: Res<LodCameraSettings>,
	loaders: Query<(&GlobalTransform, &NodeBounds, &MainRenderingLoader)>,
	mut gizmos: Gizmos,
) {
	for (transform, node_bounds, loader) in &loaders {
		if settings.show_regions {
			for band in loader.regions() {
				// Only the part of a band with data is interesting.
				let Some(region) = node_bounds.region().intersection(band.outer) else { continue };
				gizmos.cube(*transform * region_transform(region.into()), lod_color(band.lod));
			}
		}
		if settings.show_pending_tiles {
			for key in loader.pending_tiles() {
				gizmos.cube(*transform * region_transform(key.tile_key.region.into()), Color::WHITE);
			}
		}
	}
}

fn region_transform(region: Region) -> Transform {
	let size = region.size().as_vec3();
	Transform::from_translation(region.min().as_vec3() + size / 2.0).with_scale(size)
}

fn update_text(
	settings: Res<LodCameraSettings>,
	loaders: Query<&MainRenderingLoader>,
	mut texts: Query<&mut Text, With<LodCameraText>>,
) {
	let (mut desired, mut loaded, mut pending) = (0, 0, 0);
	for loader in &loaders {
		desired += loader.desired_count();
		loaded += loader.loaded_tiles().count();
		pending += loader.pending_tiles().count();
	}
	for mut text in &mut texts {
		text.0 = format!(
			"desired {desired}  loaded {loaded}  pending {pending}{}\n\
			[F] freeze regions  [G] show regions  [T] show pending tiles\n\
			[Right mouse] look  [WASD QE] move  [Shift] fast  [Scroll] speed",
			if settings.frozen { "  (frozen)" } else { "" },
		);
	}
}

fn spawn_text(mut commands: Commands) {
	commands.spawn((
		LodCameraText,
		Text::default(),
		TextFont { font_size: FontSize::Px(16.0), ..default() },
		Node { position_type: PositionType::Absolute, top: px(8), left: px(8), ..default() },
	));
}

pub struct LodCameraPlugin;

impl Plugin for LodCameraPlugin {
	fn build(&self, app: &mut App) {
		app.init_resource::<LodCameraSettings>()
			.add_systems(Startup, spawn_text)
			.add_systems(Update, (handle_input, set_loader_regions, draw_regions, update_text).chain());
	}
}
