use bevy::ecs::system::RunSystemOnce;
use bile_data::{BileDataPlugin, diff::{self, BileRegisterDiff}, simple::{self, BileRegisterSimple}};

use crate::*;

#[derive(Component, Clone, Debug, PartialEq)]
struct Value(u32);

struct Num;

impl DataType for Num {
	type Params = u32;
	type Data = Option<Value>;
	type Diff = u32;
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

impl simple::DataProducingNode<Num> for Producer {
	fn cost(&self, _request: &Request<Num>) -> Option<f32> { Some(1.0) }
	fn request(&mut self, request: Request<Num>) { self.requests.push(request.params); }
	fn drop(&mut self, params: &u32) { self.drops.push(*params); }
}

fn setup() -> (App, Entity, Entity) {
	let mut app = App::new();
	app.add_plugins(BileDataPlugin);
	app.add_component_data::<Num>()
		.add_component_data::<OtherNum>()
		.register_producer_simple::<Num, Producer>();
	let producer = app.world_mut().spawn(Producer::default()).id();
	let entity = app.world_mut().spawn_empty().id();
	(app, producer, entity)
}

fn pass(app: &mut App, producer: Entity, params: u32, data: Option<Value>) {
	app.world_mut()
		.run_system_once(move |mut passer: simple::DataPasser<Num>| passer.pass_data(producer, &params, DirtyState::UpToDate, data.clone()))
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

#[derive(Component)]
struct Adder;

impl simple::DataProducingNode<Num> for Adder {
	fn cost(&self, _request: &Request<Num>) -> Option<f32> { None }
	fn request(&mut self, _request: Request<Num>) { unreachable!() }
	fn drop(&mut self, _params: &u32) {}
}

impl diff::DiffApplyingNode<Num> for Adder {
	fn diff_cost(&self, _request: &Request<Num>, _diffs: &[u32]) -> Option<f32> { Some(0.5) }
	fn apply(&mut self, _params: &u32, data: &mut Option<Value>, diffs: Vec<u32>) -> bool {
		data.as_mut().unwrap().0 += diffs.iter().sum::<u32>();
		true
	}
}

fn dirty(app: &mut App, diff: Option<u32>) {
	app.world_mut().run_system_once(move |mut dirtier: simple::DataDirtier<Num>| dirtier.mark_dirty(&3, diff.as_ref())).unwrap();
}

#[test]
fn diffs_are_applied_to_the_component() {
	let (mut app, producer, entity) = setup();
	app.register_diff_applier::<Num, Adder>();
	app.world_mut().spawn(Adder);
	want::<Num>(&mut app.world_mut().entity_mut(entity), 3);
	app.update();
	pass(&mut app, producer, 3, Some(Value(30)));
	app.update();
	dirty(&mut app, Some(5));
	dirty(&mut app, Some(2));
	app.update();
	assert_eq!(app.world().get::<Value>(entity), Some(&Value(37)));
	assert_eq!(app.world().get::<Producer>(producer).unwrap().requests, vec![3]);
	// a dirty without a diff ends the run and the data is requested again
	dirty(&mut app, Some(1));
	dirty(&mut app, None);
	app.update();
	assert_eq!(app.world().get::<Value>(entity), Some(&Value(37)));
	assert_eq!(app.world().get::<Producer>(producer).unwrap().requests, vec![3, 3]);
	assert!(app.world().get::<Producer>(producer).unwrap().drops.is_empty());
}

#[derive(Component, Default)]
struct Consumer(Vec<Option<Value>>);

impl simple::DataConsumingNode<Num> for Consumer {
	fn pass_data(&mut self, _params: &u32, _tick: u64, data: Option<Value>) { self.0.push(data); }
	fn dirty_data(&mut self, _params: &u32, _tick: u64, _diff: Option<&u32>) {}
}

fn request(app: &mut App, consumer: Entity) -> Option<Option<Value>> {
	app.world_mut().run_system_once(move |mut requester: simple::DataRequester<Num>| requester.request(Request::new(3, DirtyState::UpToDate), consumer)).unwrap()
}

#[test]
fn nodes_share_the_data_with_the_components() {
	let (mut app, producer, entity) = setup();
	app.register_consumer_simple::<Num, Consumer>();
	let consumer = app.world_mut().spawn(Consumer::default()).id();
	want::<Num>(&mut app.world_mut().entity_mut(entity), 3);
	app.update();
	pass(&mut app, producer, 3, Some(Value(30)));
	app.update();
	// a node gets the data from the component
	assert_eq!(request(&mut app, consumer), Some(Some(Value(30))));
	// the last want takes the data out of its component, the node still uses it
	unwant::<Num>(&mut app.world_mut().entity_mut(entity));
	app.update();
	assert!(app.world().get::<Value>(entity).is_none());
	let other = app.world_mut().spawn(Consumer::default()).id();
	assert_eq!(request(&mut app, other), Some(Some(Value(30))));
	// wanted again, it is moved back into a component
	want::<Num>(&mut app.world_mut().entity_mut(entity), 3);
	app.update();
	assert_eq!(app.world().get::<Value>(entity), Some(&Value(30)));
	assert_eq!(app.world().get::<Producer>(producer).unwrap().requests, vec![3]);
	assert!(app.world().get::<Producer>(producer).unwrap().drops.is_empty());
	// dropped when nothing uses it anymore
	app.world_mut().run_system_once(move |mut requester: simple::DataRequester<Num>| { requester.cancel(&3, consumer); requester.cancel(&3, other); }).unwrap();
	unwant::<Num>(&mut app.world_mut().entity_mut(entity));
	app.update();
	assert_eq!(app.world().get::<Producer>(producer).unwrap().drops, vec![3]);
}

#[test]
fn component_data_can_be_added_after_its_nodes() {
	let mut app = App::new();
	app.add_plugins(BileDataPlugin);
	app.register_producer_simple::<Num, Producer>().add_component_data::<Num>();
	let producer = app.world_mut().spawn(Producer::default()).id();
	let entity = app.world_mut().spawn_empty().id();
	want::<Num>(&mut app.world_mut().entity_mut(entity), 3);
	app.update();
	assert_eq!(app.world().get::<Producer>(producer).unwrap().requests, vec![3]);
	pass(&mut app, producer, 3, Some(Value(30)));
	app.update();
	assert_eq!(app.world().get::<Value>(entity), Some(&Value(30)));
}
