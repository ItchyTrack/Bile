use bevy::{ecs::system::RunSystemOnce, prelude::*};
use bevy_trait_query::RegisterExt;
use bile_data::{BileAddDataType, BileDataPlugin, DataPasser, DataProducingNode, DataType, DirtyState, Request};
use bile_math::{NonZeroRegion, Region};
use bile_scene::NodeBounds;
use bile_tiles::TileKey;

use crate::{LodBand, RegionLoaderComponent, RegionLoaderPlugin};

struct Tile;

impl DataType for Tile {
	type Params = TileKey;
	type Data = Option<Marker>;
}

#[derive(Component, Clone)]
#[require(Transform)]
struct Marker;

#[derive(Component, Default)]
struct Producer {
	requests: Vec<TileKey>,
	drops: Vec<TileKey>,
}

impl DataProducingNode<Tile> for Producer {
	fn cost(&self, _request: &Request<Tile>) -> Option<f32> { Some(1.0) }
	fn request(&mut self, request: Request<Tile>) { self.requests.push(request.params); }
	fn drop(&mut self, params: &TileKey) { self.drops.push(*params); }
}

fn band(lod: u8, size: u32) -> LodBand {
	LodBand { lod, outer: Region::new(IVec3::ZERO, UVec3::splat(size)), inner: None }
}

fn pass(app: &mut App, producer: Entity, key: TileKey, data: Option<Marker>) {
	app.world_mut()
		.run_system_once(move |mut passer: DataPasser<Tile>| passer.pass_data(producer, &key, DirtyState::UpToDate, data.clone()))
		.unwrap();
}

fn spawned_markers(app: &mut App) -> usize {
	app.world_mut().query_filtered::<(), With<Marker>>().iter(app.world()).count()
}

#[test]
fn coarse_tile_is_kept_until_finer_tiles_load() {
	let mut app = App::new();
	app.add_plugins((BileDataPlugin, RegionLoaderPlugin::<Tile>::default()))
		.bile_add_data_type::<Tile>()
		.register_component_as::<dyn DataProducingNode<Tile>, Producer>();
	let producer = app.world_mut().spawn(Producer::default()).id();
	let bounds = NodeBounds::new(NonZeroRegion::new(IVec3::ZERO, UVec3::splat(4)).unwrap());
	let mut loader = RegionLoaderComponent::<Tile>::new(1);
	loader.set_regions(vec![band(1, 2)]);
	let loader = app.world_mut().spawn((bounds, loader)).id();

	app.update();
	let coarse = app.world().get::<Producer>(producer).unwrap().requests.clone();
	assert_eq!(coarse.len(), 1);
	pass(&mut app, producer, coarse[0], Some(Marker));
	app.update();
	assert_eq!(spawned_markers(&mut app), 1);

	app.world_mut().get_mut::<RegionLoaderComponent<Tile>>(loader).unwrap().set_regions(vec![band(0, 2)]);
	app.update();
	let fine = app.world().get::<Producer>(producer).unwrap().requests[1..].to_vec();
	assert_eq!(fine.len(), 8);
	for (i, &key) in fine[..7].iter().enumerate() {
		pass(&mut app, producer, key, (i % 2 == 0).then_some(Marker));
	}
	app.update();
	assert_eq!(spawned_markers(&mut app), 1 + 4);
	assert!(app.world().get::<RegionLoaderComponent<Tile>>(loader).unwrap().is_loaded(coarse[0]));

	pass(&mut app, producer, fine[7], None);
	app.update();
	assert_eq!(spawned_markers(&mut app), 4);
	assert!(!app.world().get::<RegionLoaderComponent<Tile>>(loader).unwrap().is_loaded(coarse[0]));
	app.update();
	assert_eq!(app.world().get::<Producer>(producer).unwrap().drops, coarse);
}

#[test]
fn loaded_tile_is_positioned_in_the_same_frame() {
	let mut app = App::new();
	app.add_plugins((TransformPlugin, BileDataPlugin, RegionLoaderPlugin::<Tile>::default()))
		.bile_add_data_type::<Tile>()
		.register_component_as::<dyn DataProducingNode<Tile>, Producer>();
	let producer = app.world_mut().spawn(Producer::default()).id();
	let bounds = NodeBounds::new(NonZeroRegion::new(IVec3::ZERO, UVec3::splat(4)).unwrap());
	let mut loader = RegionLoaderComponent::<Tile>::new(1);
	loader.set_regions(vec![band(0, 1)]);
	app.world_mut().spawn((Transform::from_xyz(10.0, 0.0, 0.0), bounds, loader));

	app.update();
	let key = app.world().get::<Producer>(producer).unwrap().requests[0];
	pass(&mut app, producer, key, Some(Marker));
	app.update();
	let mut tiles = app.world_mut().query_filtered::<&GlobalTransform, With<Marker>>();
	assert_eq!(tiles.single(app.world()).unwrap().translation(), Vec3::new(10.0, 0.0, 0.0));
}

#[test]
fn removing_loader_cancels_pending_requests() {
	let mut app = App::new();
	app.add_plugins((BileDataPlugin, RegionLoaderPlugin::<Tile>::default()))
		.bile_add_data_type::<Tile>()
		.register_component_as::<dyn DataProducingNode<Tile>, Producer>();
	let producer = app.world_mut().spawn(Producer::default()).id();
	let bounds = NodeBounds::new(NonZeroRegion::new(IVec3::ZERO, UVec3::splat(4)).unwrap());
	let mut loader = RegionLoaderComponent::<Tile>::new(1);
	loader.set_regions(vec![band(0, 1)]);
	let loader = app.world_mut().spawn((bounds, loader)).id();

	app.update();
	let requests = app.world().get::<Producer>(producer).unwrap().requests.clone();
	assert_eq!(requests.len(), 1);
	app.world_mut().entity_mut(loader).remove::<RegionLoaderComponent<Tile>>();
	app.update();
	app.update();
	assert_eq!(app.world().get::<Producer>(producer).unwrap().drops, requests);
}
