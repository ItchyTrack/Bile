use bevy::{prelude::*, window::PrimaryWindow};

use crate::{heightmap::{HeightField, HeightmapSystems}, lod_camera::LodCamera};

const MAX_DISTANCE: f32 = 20_000.0;
const MIN_RADIUS: f32 = 4.0;
const MAX_RADIUS: f32 = 256.0;

#[derive(Resource)]
struct Brush {
	radius: f32,
	rate: f32,
}

#[derive(Component)]
struct TerraformText;

fn cast(field: &HeightField, ray: Ray3d) -> Option<Vec3> {
	let above = |t: f32| {
		let point = ray.get_point(t);
		point.y - field.height(point.xz())
	};
	let (mut near, mut t) = (0.0, 0.0);
	while t < MAX_DISTANCE {
		let gap = above(t);
		if gap <= 0.0 {
			for _ in 0..16 {
				let middle = (near + t) / 2.0;
				if above(middle) > 0.0 { near = middle } else { t = middle }
			}
			return Some(ray.get_point(t));
		}
		near = t;
		t += (gap * 0.5).max(0.5);
	}
	None
}

fn terraform(
	time: Res<Time>,
	mouse: Res<ButtonInput<MouseButton>>,
	keys: Res<ButtonInput<KeyCode>>,
	mut brush: ResMut<Brush>,
	mut field: ResMut<HeightField>,
	window: Single<&Window, With<PrimaryWindow>>,
	camera: Single<(&Camera, &GlobalTransform), With<LodCamera>>,
	mut gizmos: Gizmos,
) {
	if keys.just_pressed(KeyCode::BracketLeft) { brush.radius = (brush.radius / 1.25).max(MIN_RADIUS); }
	if keys.just_pressed(KeyCode::BracketRight) { brush.radius = (brush.radius * 1.25).min(MAX_RADIUS); }

	let (camera, camera_transform) = camera.into_inner();
	let Some(cursor) = window.cursor_position() else { return };
	let Ok(ray) = camera.viewport_to_world(camera_transform, cursor) else { return };
	let Some(hit) = cast(&field, ray) else { return };
	gizmos.circle(Isometry3d::new(hit + Vec3::Y * 0.5, Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)), brush.radius, Color::WHITE);

	if !mouse.pressed(MouseButton::Left) { return; }
	let lower = keys.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight]);
	let amount = brush.rate * brush.radius * time.delta_secs() * if lower { -1.0 } else { 1.0 };
	field.raise(hit.xz(), brush.radius, amount);
}

fn update_text(brush: Res<Brush>, mut texts: Query<&mut Text, With<TerraformText>>) {
	if !brush.is_changed() { return; }
	for mut text in &mut texts {
		text.0 = format!("[Left mouse] raise  [Ctrl + Left mouse] lower  [ ] brush radius {:.0}", brush.radius);
	}
}

fn spawn_text(mut commands: Commands) {
	commands.spawn((
		TerraformText,
		Text::default(),
		TextFont { font_size: FontSize::Px(16.0), ..default() },
		Node { position_type: PositionType::Absolute, bottom: px(8), left: px(8), ..default() },
	));
}

pub struct TerraformPlugin;

impl Plugin for TerraformPlugin {
	fn build(&self, app: &mut App) {
		app.insert_resource(Brush { radius: 24.0, rate: 1.0 })
			.add_systems(Startup, spawn_text)
			.add_systems(Update, (terraform, update_text).before(HeightmapSystems));
	}
}
