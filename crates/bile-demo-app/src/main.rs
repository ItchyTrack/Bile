use bevy::{
	camera_controller::free_camera::{FreeCamera, FreeCameraPlugin},
	prelude::*,
};
use bile_region_loader::{RegionLoaderComponent, RegionLoaderPlugin};

mod grass;
mod lod_camera;
mod terrain;

pub struct MainRendering;
pub type MainRenderingLoader = RegionLoaderComponent<MainRendering>;

fn main() {
	App::new()
		.add_plugins(DefaultPlugins)
		.add_plugins(FreeCameraPlugin)
		.add_plugins(bile::DefaultBilePlugins)
		.add_plugins(RegionLoaderPlugin::<MainRendering>::default())
		.add_plugins((terrain::TerrainPlugin, grass::GrassPlugin, lod_camera::LodCameraPlugin))
		.insert_resource(ClearColor(Color::srgb(0.6, 0.75, 0.9)))
		.insert_resource(GlobalAmbientLight { brightness: 400.0, ..default() })
		.add_systems(Startup, scene.spawn())
		.run();
}

fn scene() -> impl SceneList {
	bsn_list! [
		(
			DirectionalLight {
				illuminance: 8000.0,
			}
			template_value(Transform::from_xyz(1.0, 2.0, 0.5).looking_at(Vec3::ZERO, Vec3::Y))
		),
		(
			Camera3d
			lod_camera::LodCamera
			template_value(FreeCamera { walk_speed: 60.0, run_speed: 600.0, ..default() })
			template_value(Projection::Perspective(PerspectiveProjection { far: 100_000.0, ..default() }))
			template_value(DistanceFog {
				color: Color::srgb(0.6, 0.75, 0.9),
				falloff: FogFalloff::Linear { start: 4000.0, end: 30000.0 },
				..default()
			})
			template_value(Transform::from_xyz(0.0, 250.0, 0.0).looking_at(Vec3::new(400.0, 0.0, 400.0), Vec3::Y))
		)
	]
}
