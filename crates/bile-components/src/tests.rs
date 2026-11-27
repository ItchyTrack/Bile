use bevy::ecs::system::RunSystemOnce;
use bile_data::{
	BileDataPlugin,
	simple::diff::{BileRegisterDiff, DiffApplyingNode},
	simple::consuming::{BileRegisterSimpleConsumer, ConsumingNodeAPI, Requester},
	simple::producing::{BileRegisterSimpleProducer, DataDirtier, DataPasser, ProducingNodeAPI},
};

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
	cancels: Vec<(u32, DirtyState, Option<u64>)>,
}

impl ProducingNodeAPI<Num> for Producer {
	fn cost(&self, _request: &Request<Num>) -> Option<f32> { Some(1.0) }
	fn request(&mut self, request: Request<Num>) { self.requests.push(request.params); }
	fn cancel(&mut self, params: &u32, latest: DirtyState, oldest: Option<u64>) { self.cancels.push((*params, latest, oldest)); }
}

const DROP: (u32, DirtyState, Option<u64>) = (3, DirtyState::UpToDate, None);

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
		.run_system_once(move |mut passer: DataPasser<Num>| passer.pass_data(producer, &params, DirtyState::UpToDate, data.clone()))
		.unwrap();
}

fn dirty(app: &mut App, diff: Option<u32>) {
	app.world_mut()
		.run_system_once(move |mut dirtier: DataDirtier<Num>| {
			let tick = dirtier.current_tick();
			dirtier.mark_dirty(&3, tick, diff.as_ref());
		})
		.unwrap();
}

fn resolved(app: &mut App) -> Vec<Entity> {
	app.world_mut().resource_mut::<Messages<ComponentResolved>>().drain().map(|resolved| resolved.entity).collect()
}

fn value_id(app: &mut App) -> ComponentId {
	app.world_mut().register_component::<Value>()
}

fn producer(app: &App, producer: Entity) -> &Producer {
	app.world().get::<Producer>(producer).unwrap()
}

#[test]
fn wanted_component_is_inserted_when_delivered() {
	let (mut app, source, entity) = setup();
	want::<Num>(&mut app.world_mut().entity_mut(entity), 3);
	app.update();
	assert_eq!(producer(&app, source).requests, vec![3]);
	assert!(resolved(&mut app).is_empty());

	pass(&mut app, source, 3, Some(Value(30)));
	app.update();
	assert_eq!(app.world().get::<Value>(entity), Some(&Value(30)));
	assert!(app.world().get::<Want<Num>>(entity).unwrap().is_resolved());
	assert_eq!(resolved(&mut app), vec![entity]);
}

#[test]
fn empty_data_resolves_without_inserting() {
	let (mut app, source, entity) = setup();
	want::<Num>(&mut app.world_mut().entity_mut(entity), 3);
	app.update();
	pass(&mut app, source, 3, None);
	app.update();
	assert!(app.world().get::<Value>(entity).is_none());
	assert!(app.world().get::<Want<Num>>(entity).unwrap().is_resolved());
	assert_eq!(resolved(&mut app), vec![entity]);
}

#[test]
fn dirty_component_is_requested_again_and_replaced() {
	let (mut app, source, entity) = setup();
	want::<Num>(&mut app.world_mut().entity_mut(entity), 3);
	app.update();
	pass(&mut app, source, 3, Some(Value(30)));
	app.update();
	resolved(&mut app);
	dirty(&mut app, None);
	app.update();
	assert_eq!(producer(&app, source).requests, vec![3, 3]);
	// the old data stays until the new data is there
	assert_eq!(app.world().get::<Value>(entity), Some(&Value(30)));
	pass(&mut app, source, 3, Some(Value(31)));
	app.update();
	assert_eq!(app.world().get::<Value>(entity), Some(&Value(31)));
	assert!(resolved(&mut app).is_empty());
	assert!(producer(&app, source).cancels.is_empty());
}

#[test]
fn a_new_want_waits_for_up_to_date_data() {
	let (mut app, source, entity) = setup();
	want::<Num>(&mut app.world_mut().entity_mut(entity), 3);
	app.update();
	pass(&mut app, source, 3, Some(Value(30)));
	app.update();
	resolved(&mut app);
	dirty(&mut app, None);
	let other = app.world_mut().spawn_empty().id();
	want::<Num>(&mut app.world_mut().entity_mut(other), 3);
	app.update();
	assert!(app.world().get::<Value>(other).is_none());
	assert!(resolved(&mut app).is_empty());
	pass(&mut app, source, 3, Some(Value(31)));
	app.update();
	assert_eq!(app.world().get::<Value>(other), Some(&Value(31)));
	assert_eq!(resolved(&mut app), vec![other]);
}

#[test]
fn moving_a_want_keeps_the_data() {
	let (mut app, source, entity) = setup();
	let other = app.world_mut().spawn_empty().id();
	want::<Num>(&mut app.world_mut().entity_mut(entity), 3);
	app.update();
	pass(&mut app, source, 3, Some(Value(30)));
	app.update();
	unwant::<Num>(&mut app.world_mut().entity_mut(entity));
	want::<Num>(&mut app.world_mut().entity_mut(other), 3);
	app.update();
	assert_eq!(app.world().get::<Value>(other), Some(&Value(30)));
	assert_eq!(producer(&app, source).requests, vec![3]);
	assert!(producer(&app, source).cancels.is_empty());
}

#[test]
fn want_is_reference_counted() {
	let (mut app, source, entity) = setup();
	want::<Num>(&mut app.world_mut().entity_mut(entity), 3);
	want::<Num>(&mut app.world_mut().entity_mut(entity), 3);
	app.update();
	pass(&mut app, source, 3, Some(Value(30)));
	app.update();

	unwant::<Num>(&mut app.world_mut().entity_mut(entity));
	app.update();
	app.update();
	assert_eq!(app.world().get::<Value>(entity), Some(&Value(30)));
	assert!(producer(&app, source).cancels.is_empty());

	let value = value_id(&mut app);
	unwant_by_id(&mut app.world_mut().entity_mut(entity), value);
	app.update();
	app.update();
	assert!(app.world().get::<Value>(entity).is_none());
	assert!(app.world().get::<Want<Num>>(entity).is_none());
	assert_eq!(producer(&app, source).cancels, vec![DROP]);
}

#[test]
fn unwanting_the_last_holder_drops_the_data() {
	let (mut app, source, entity) = setup();
	let other = app.world_mut().spawn_empty().id();
	want::<Num>(&mut app.world_mut().entity_mut(entity), 3);
	want::<Num>(&mut app.world_mut().entity_mut(other), 3);
	app.update();
	assert_eq!(producer(&app, source).requests, vec![3]);
	pass(&mut app, source, 3, Some(Value(30)));
	app.update();
	unwant::<Num>(&mut app.world_mut().entity_mut(entity));
	app.update();
	assert!(app.world().get::<Value>(entity).is_none());
	assert_eq!(app.world().get::<Value>(other), Some(&Value(30)));
	assert!(producer(&app, source).cancels.is_empty());
	unwant::<Num>(&mut app.world_mut().entity_mut(other));
	app.update();
	assert_eq!(producer(&app, source).cancels, vec![DROP]);
	// so it is requested again
	want::<Num>(&mut app.world_mut().entity_mut(entity), 3);
	app.update();
	assert_eq!(producer(&app, source).requests, vec![3, 3]);
}

#[test]
fn despawning_cancels_the_request() {
	let (mut app, source, entity) = setup();
	want::<Num>(&mut app.world_mut().entity_mut(entity), 3);
	app.update();
	app.world_mut().despawn(entity);
	app.update();
	pass(&mut app, source, 3, Some(Value(30)));
	app.update();
	assert_eq!(producer(&app, source).cancels, vec![DROP]);
}

#[test]
#[should_panic(expected = "one data type")]
fn component_can_only_come_from_one_data_type() {
	let (mut app, _, entity) = setup();
	want::<Num>(&mut app.world_mut().entity_mut(entity), 3);
	want::<OtherNum>(&mut app.world_mut().entity_mut(entity), 3);
}

#[derive(Component)]
struct Adder(bool); // if it applies the diffs right away

impl DiffApplyingNode<Num> for Adder {
	fn diff_cost(&self, _request: &Request<Num>, _diffs: &[u32]) -> Option<f32> { Some(0.5) }
	fn apply(&mut self, _params: &u32, data: &mut Option<Value>, diffs: Vec<u32>) -> bool {
		if self.0 { data.as_mut().unwrap().0 += diffs.iter().sum::<u32>(); }
		self.0
	}
}

#[test]
fn diffs_are_applied_to_the_component() {
	let (mut app, source, entity) = setup();
	app.register_diff_applier::<Num, Adder>();
	app.world_mut().spawn(Adder(true));
	want::<Num>(&mut app.world_mut().entity_mut(entity), 3);
	app.update();
	pass(&mut app, source, 3, Some(Value(30)));
	app.update();
	dirty(&mut app, Some(5));
	dirty(&mut app, Some(2));
	app.update();
	assert_eq!(app.world().get::<Value>(entity), Some(&Value(37)));
	assert_eq!(producer(&app, source).requests, vec![3]);
	// the diffs can not be applied past a dirty without a diff, so the data is requested again
	dirty(&mut app, Some(1));
	dirty(&mut app, None);
	app.update();
	assert_eq!(app.world().get::<Value>(entity), Some(&Value(37)));
	assert_eq!(producer(&app, source).requests, vec![3, 3]);
	assert!(producer(&app, source).cancels.is_empty());
}

#[test]
fn diffs_applied_later_reach_the_component() {
	let (mut app, source, entity) = setup();
	app.register_diff_applier::<Num, Adder>();
	let adder = app.world_mut().spawn(Adder(false)).id();
	want::<Num>(&mut app.world_mut().entity_mut(entity), 3);
	app.update();
	pass(&mut app, source, 3, Some(Value(30)));
	app.update();
	dirty(&mut app, Some(5));
	app.update();
	assert_eq!(producer(&app, source).requests, vec![3]);
	app.world_mut().run_system_once(move |mut passer: bile_data::simple::diff::DataPasser<Num>| passer.pass_data(adder, &3, Some(Value(35)))).unwrap();
	app.update();
	assert_eq!(app.world().get::<Value>(entity), Some(&Value(35)));
}

#[derive(Component, Default)]
struct Consumer(Vec<Option<Value>>);

impl ConsumingNodeAPI<Num> for Consumer {
	fn pass_data(&mut self, _params: &u32, _dirty_state: DirtyState, data: &Option<Value>) { self.0.push(data.clone()); }
	fn dirty_data(&mut self, _params: &u32, _last_clean_tick: u64, _diff: Option<&u32>) {}
}

fn request(app: &mut App, consumer: Entity) -> Option<Option<Value>> {
	app.world_mut()
		.run_system_once(move |mut requester: Requester<Num>| requester.request(consumer, &Request::new(3, DirtyState::UpToDate)).map(|(data, _)| data))
		.unwrap()
}

fn cancel(app: &mut App, consumer: Entity) {
	app.world_mut().run_system_once(move |mut requester: Requester<Num>| requester.cancel(consumer, &3, DirtyState::UpToDate, None)).unwrap();
}

#[test]
fn nodes_share_the_data_with_the_components() {
	let (mut app, source, entity) = setup();
	app.register_consumer_simple::<Num, Consumer>();
	let consumer = app.world_mut().spawn(Consumer::default()).id();
	want::<Num>(&mut app.world_mut().entity_mut(entity), 3);
	app.update();
	pass(&mut app, source, 3, Some(Value(30)));
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
	assert_eq!(producer(&app, source).requests, vec![3]);
	assert!(producer(&app, source).cancels.is_empty());
	// dropped when nothing uses it anymore
	cancel(&mut app, consumer);
	cancel(&mut app, other);
	app.update();
	assert!(producer(&app, source).cancels.is_empty());
	unwant::<Num>(&mut app.world_mut().entity_mut(entity));
	app.update();
	assert_eq!(producer(&app, source).cancels, vec![DROP]);
}

#[test]
fn a_node_waiting_for_wanted_data_gets_it() {
	let (mut app, source, entity) = setup();
	app.register_consumer_simple::<Num, Consumer>();
	let consumer = app.world_mut().spawn(Consumer::default()).id();
	want::<Num>(&mut app.world_mut().entity_mut(entity), 3);
	app.update();
	assert_eq!(request(&mut app, consumer), None);
	assert_eq!(producer(&app, source).requests, vec![3]);
	pass(&mut app, source, 3, Some(Value(30)));
	assert_eq!(app.world().get::<Consumer>(consumer).unwrap().0, vec![Some(Value(30))]);
	app.update();
	assert_eq!(app.world().get::<Value>(entity), Some(&Value(30)));
	// the want is gone, the node still uses the data
	unwant::<Num>(&mut app.world_mut().entity_mut(entity));
	app.update();
	assert!(producer(&app, source).cancels.is_empty());
	cancel(&mut app, consumer);
	assert_eq!(producer(&app, source).cancels, vec![DROP]);
}

#[test]
fn a_want_keeps_the_data_a_node_no_longer_uses() {
	let (mut app, source, entity) = setup();
	app.register_consumer_simple::<Num, Consumer>();
	let consumer = app.world_mut().spawn(Consumer::default()).id();
	assert_eq!(request(&mut app, consumer), None);
	pass(&mut app, source, 3, Some(Value(30)));
	// wanted and no longer used by the node in the same frame
	want::<Num>(&mut app.world_mut().entity_mut(entity), 3);
	cancel(&mut app, consumer);
	app.update();
	assert_eq!(app.world().get::<Value>(entity), Some(&Value(30)));
	assert!(producer(&app, source).cancels.is_empty());
}

#[test]
fn component_data_can_be_added_after_its_nodes() {
	let mut app = App::new();
	app.add_plugins(BileDataPlugin);
	app.register_producer_simple::<Num, Producer>().add_component_data::<Num>();
	let source = app.world_mut().spawn(Producer::default()).id();
	let entity = app.world_mut().spawn_empty().id();
	want::<Num>(&mut app.world_mut().entity_mut(entity), 3);
	app.update();
	assert_eq!(producer(&app, source).requests, vec![3]);
	pass(&mut app, source, 3, Some(Value(30)));
	app.update();
	assert_eq!(app.world().get::<Value>(entity), Some(&Value(30)));
}
