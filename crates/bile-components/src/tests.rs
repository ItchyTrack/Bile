use bevy::ecs::system::RunSystemOnce;
use bile_data::{BileDataPlugin, single_cached::{self, BileRegisterSingleCached}};

use crate::*;

#[derive(Component, Clone, Debug, PartialEq)]
struct Value(u32);

struct Num;

impl DataType for Num {
	type Params = u32;
	type Data = Option<Value>;
	type Diff = ();
}

impl ComponentData for Num {
	type Component = Value;
}

struct OtherNum;

impl DataType for OtherNum {
	type Params = u32;
	type Data = Option<Value>;
	type Diff = ();
}

impl ComponentData for OtherNum {
	type Component = Value;
}

#[derive(Component, Default)]
struct Producer {
	requests: Vec<u32>,
	drops: Vec<u32>,
}

impl single_cached::DataProducingNode<Num> for Producer {
	fn cost(&self, _request: &Request<Num>) -> Option<f32> { Some(1.0) }
	fn request(&mut self, request: Request<Num>) { self.requests.push(request.params); }
	fn drop(&mut self, params: &u32) { self.drops.push(*params); }
}

fn setup() -> (App, Entity, Entity) {
	let mut app = App::new();
	app.add_plugins(BileDataPlugin);
	app.add_component_data::<Num>()
		.add_component_data::<OtherNum>()
		.register_producer_single_cached::<Num, Producer>();
	let producer = app.world_mut().spawn(Producer::default()).id();
	let entity = app.world_mut().spawn_empty().id();
	(app, producer, entity)
}

fn pass(app: &mut App, producer: Entity, params: u32, data: Option<Value>) {
	app.world_mut()
		.run_system_once(move |mut passer: single_cached::DataPasser<Num>| passer.pass_data(producer, &params, DirtyState::UpToDate, data.clone()))
		.unwrap();
}

fn value_id(app: &mut App) -> ComponentId {
	app.world_mut().register_component::<Value>()
}

#[test]
fn wanted_component_is_inserted_when_delivered() {
	let (mut app, producer, entity) = setup();
	want::<Num>(&mut app.world_mut().entity_mut(entity), 3);
	app.update();
	assert_eq!(app.world().get::<Producer>(producer).unwrap().requests, vec![3]);

	pass(&mut app, producer, 3, Some(Value(30)));
	app.update();
	assert_eq!(app.world().get::<Value>(entity), Some(&Value(30)));
	assert!(app.world().get::<Want<Num>>(entity).unwrap().is_resolved());
}

#[test]
fn empty_data_resolves_without_inserting() {
	let (mut app, producer, entity) = setup();
	want::<Num>(&mut app.world_mut().entity_mut(entity), 3);
	app.update();
	pass(&mut app, producer, 3, None);
	app.update();
	assert!(app.world().get::<Value>(entity).is_none());
	assert!(app.world().get::<Want<Num>>(entity).unwrap().is_resolved());
}

#[test]
fn want_is_reference_counted() {
	let (mut app, producer, entity) = setup();
	want::<Num>(&mut app.world_mut().entity_mut(entity), 3);
	want::<Num>(&mut app.world_mut().entity_mut(entity), 3);
	app.update();
	pass(&mut app, producer, 3, Some(Value(30)));
	app.update();

	unwant::<Num>(&mut app.world_mut().entity_mut(entity));
	app.update();
	app.update();
	assert_eq!(app.world().get::<Value>(entity), Some(&Value(30)));
	assert!(app.world().get::<Producer>(producer).unwrap().drops.is_empty());

	let value = value_id(&mut app);
	unwant_by_id(&mut app.world_mut().entity_mut(entity), value);
	app.update();
	app.update();
	assert!(app.world().get::<Value>(entity).is_none());
	assert!(app.world().get::<Want<Num>>(entity).is_none());
	assert_eq!(app.world().get::<Producer>(producer).unwrap().drops, vec![3]);
}

#[test]
fn despawning_cancels_the_request() {
	let (mut app, producer, entity) = setup();
	want::<Num>(&mut app.world_mut().entity_mut(entity), 3);
	app.update();
	app.world_mut().despawn(entity);
	app.update();
	pass(&mut app, producer, 3, Some(Value(30)));
	app.update();
	assert_eq!(app.world().get::<Producer>(producer).unwrap().drops, vec![3]);
}

#[test]
#[should_panic(expected = "one data type")]
fn component_can_only_come_from_one_data_type() {
	let (mut app, _, entity) = setup();
	want::<Num>(&mut app.world_mut().entity_mut(entity), 3);
	want::<OtherNum>(&mut app.world_mut().entity_mut(entity), 3);
}
