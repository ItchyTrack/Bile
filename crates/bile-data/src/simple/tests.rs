use bevy::{ecs::{resource::IsResource, system::RunSystemOnce}, prelude::*};
use bevy_trait_query::{One, RegisterExt};

use super::{diff::{BileRegisterDiff, DiffApplyingNode}, api::{DynFulfillerParams, DynRequesterParams, DynStorageFulfillerParams, DynStorageRequesterParams, FulfillerAPI, RequesterAPI, StorageFulfillerAPI, StorageRequesterAPI}};
use super::consuming::{BileRegisterSimpleConsumer, ConsumingNodeAPI, Requester};
use super::producing::{BileRegisterSimpleProducer, DataDirtier, DataPasser, ProducingNodeAPI};
use super::storage::BileAddSimpleStorage;
use crate::{BileDataPlugin, BileDataSchedule, DataType, DirtyState, Request, StateTick, cancel_range::{in_cancel_range, unheld_ranges}};

const UP_TO_DATE: DirtyState = DirtyState::UpToDate;
const fn dirty(tick: u64) -> DirtyState { DirtyState::Dirty { tick } }

// ============================ Ranges ============================

#[test]
fn unheld_ranges_skip_the_held_states() {
	// A waits for UpToDate, B for tick 5, A cancels everything
	assert_eq!(unheld_ranges([dirty(5)], UP_TO_DATE, None), vec![(dirty(4), None), (UP_TO_DATE, Some(6))]);
	assert_eq!(unheld_ranges([], UP_TO_DATE, None), vec![(UP_TO_DATE, None)]);
	assert_eq!(unheld_ranges([UP_TO_DATE], UP_TO_DATE, None), vec![(dirty(u64::MAX), None)]);
	assert_eq!(unheld_ranges([dirty(0)], UP_TO_DATE, None), vec![(UP_TO_DATE, Some(1))]);
	assert_eq!(unheld_ranges([dirty(0), UP_TO_DATE], UP_TO_DATE, None), vec![(dirty(u64::MAX), Some(1))]);
	assert_eq!(unheld_ranges([dirty(5)], UP_TO_DATE, Some(3)), vec![(dirty(4), Some(3)), (UP_TO_DATE, Some(6))]);
	assert_eq!(unheld_ranges([dirty(3), dirty(4)], dirty(4), Some(3)), vec![]);
	// held states outside of the cancel do not matter
	assert_eq!(unheld_ranges([dirty(1), dirty(9)], dirty(7), Some(3)), vec![(dirty(7), Some(3))]);
	assert_eq!(unheld_ranges([], dirty(2), Some(3)), vec![]);
}

#[test]
fn cancel_covers_its_range() {
	assert!(in_cancel_range(UP_TO_DATE, UP_TO_DATE, Some(6)));
	assert!(in_cancel_range(dirty(6), UP_TO_DATE, Some(6)));
	assert!(!in_cancel_range(dirty(5), UP_TO_DATE, Some(6)));
	assert!(in_cancel_range(dirty(0), dirty(4), None));
	assert!(!in_cancel_range(dirty(5), dirty(4), None));
	assert!(!in_cancel_range(UP_TO_DATE, dirty(u64::MAX), None));
}

// ============================ Nodes ============================

struct Sum;
impl DataType for Sum {
	type Params = u32;
	type Data = u32;
	type Diff = u32;
}

#[derive(Component)]
struct Producer { cost: f32, requests: Vec<(u32, DirtyState)>, cancels: Vec<(u32, DirtyState, Option<u64>)> }
impl Producer {
	fn new(cost: f32) -> Self { Self { cost, requests: Vec::new(), cancels: Vec::new() } }
}
impl ProducingNodeAPI<Sum> for Producer {
	fn cost(&self, _request: &Request<Sum>) -> Option<f32> { Some(self.cost) }
	fn request(&mut self, request: Request<Sum>) { self.requests.push((request.params, request.allowed_tick)); }
	fn cancel(&mut self, params: &u32, latest: DirtyState, oldest: Option<u64>) { self.cancels.push((*params, latest, oldest)); }
}

#[derive(Component, Default)]
struct Consumer(Vec<String>);
impl ConsumingNodeAPI<Sum> for Consumer {
	fn pass_data(&mut self, params: &u32, dirty_state: DirtyState, data: &u32) { self.0.push(format!("pass {params} {dirty_state:?} {data}")); }
	fn dirty_data(&mut self, params: &u32, last_clean_tick: u64, diff: Option<&u32>) { self.0.push(format!("dirty {params} {last_clean_tick} {diff:?}")); }
}

#[derive(Component)]
struct Adder { cost: f32, applies: bool }
impl DiffApplyingNode<Sum> for Adder {
	fn diff_cost(&self, _request: &Request<Sum>, _diffs: &[u32]) -> Option<f32> { Some(self.cost) }
	fn apply(&mut self, _params: &u32, data: &mut u32, diffs: Vec<u32>) -> bool {
		if self.applies { *data += diffs.iter().sum::<u32>(); }
		self.applies
	}
}

fn sum_app() -> App {
	let mut app = App::new();
	app.add_plugins(BileDataPlugin);
	app.add_simple_storage::<Sum>().register_producer_simple::<Sum, Producer>().register_consumer_simple::<Sum, Consumer>();
	app
}

fn request(app: &mut App, node: Entity, params: u32, allowed_tick: DirtyState) -> Option<(u32, DirtyState)> {
	app.world_mut().run_system_once(move |mut requester: Requester<Sum>| requester.request(node, &Request::new(params, allowed_tick))).unwrap()
}
fn cancel(app: &mut App, node: Entity, params: u32, latest: DirtyState, oldest: Option<u64>) {
	app.world_mut().run_system_once(move |mut requester: Requester<Sum>| requester.cancel(node, &params, latest, oldest)).unwrap();
}
fn pass(app: &mut App, source: Entity, params: u32, dirty_state: DirtyState, data: u32) {
	app.world_mut().run_system_once(move |mut passer: DataPasser<Sum>| passer.pass_data(source, &params, dirty_state, data)).unwrap();
}
fn pass_diffed(app: &mut App, source: Entity, params: u32, data: u32) {
	app.world_mut().run_system_once(move |mut passer: DataPasser<Sum>| passer.pass_diffed_data(source, &params, data)).unwrap();
}
fn mark_dirty(app: &mut App, params: u32, last_clean_tick: u64, diff: Option<u32>) {
	app.world_mut().run_system_once(move |mut dirtier: DataDirtier<Sum>| dirtier.mark_dirty(&params, last_clean_tick, diff.as_ref())).unwrap();
}
fn events(app: &App, node: Entity) -> Vec<&str> {
	app.world().get::<Consumer>(node).unwrap().0.iter().map(String::as_str).collect()
}
fn producer(app: &App, node: Entity) -> &Producer {
	app.world().get::<Producer>(node).unwrap()
}

// ============================ Requesting ============================

#[test]
fn the_cheapest_node_is_asked_and_the_data_is_stored() {
	let mut app = sum_app();
	let expensive = app.world_mut().spawn(Producer::new(2.0)).id();
	let cheap = app.world_mut().spawn(Producer::new(1.0)).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	let b = app.world_mut().spawn(Consumer::default()).id();
	assert_eq!(request(&mut app, a, 3, UP_TO_DATE), None);
	assert_eq!(producer(&app, cheap).requests, vec![(3, UP_TO_DATE)]);
	assert!(producer(&app, expensive).requests.is_empty());
	// a node that was not asked passes nothing
	pass(&mut app, expensive, 3, UP_TO_DATE, 99);
	assert!(events(&app, a).is_empty());
	pass(&mut app, cheap, 3, UP_TO_DATE, 30);
	assert_eq!(events(&app, a), vec!["pass 3 UpToDate 30"]);
	assert_eq!(request(&mut app, b, 3, UP_TO_DATE), Some((30, UP_TO_DATE)));
	assert_eq!(producer(&app, cheap).requests.len(), 1);
	assert!(events(&app, b).is_empty());
}

#[test]
fn a_pass_reaches_a_node_once_for_the_requests_it_satisfies() {
	let mut app = sum_app();
	let source = app.world_mut().spawn(Producer::new(1.0)).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	assert_eq!(request(&mut app, a, 3, UP_TO_DATE), None);
	assert_eq!(request(&mut app, a, 3, dirty(5)), None);
	assert_eq!(request(&mut app, a, 3, dirty(4)), None);
	assert_eq!(producer(&app, source).requests, vec![(3, UP_TO_DATE), (3, dirty(5)), (3, dirty(4))]);
	pass(&mut app, source, 3, dirty(7), 70);
	assert_eq!(events(&app, a), vec!["pass 3 Dirty { tick: 7 } 70"]);
	pass(&mut app, source, 3, UP_TO_DATE, 80);
	assert_eq!(events(&app, a), vec!["pass 3 Dirty { tick: 7 } 70", "pass 3 UpToDate 80"]);
	// it waits for nothing anymore
	pass(&mut app, source, 3, UP_TO_DATE, 90);
	assert_eq!(events(&app, a).len(), 2);
	cancel(&mut app, a, 3, UP_TO_DATE, None);
	assert_eq!(producer(&app, source).cancels, vec![(3, UP_TO_DATE, None)]);
}

#[test]
fn dirty_reaches_the_nodes_with_the_data_and_newer_data_is_requested_again() {
	let mut app = sum_app();
	let source = app.world_mut().spawn(Producer::new(1.0)).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	let b = app.world_mut().spawn(Consumer::default()).id();
	let waiting = app.world_mut().spawn(Consumer::default()).id();
	request(&mut app, a, 3, UP_TO_DATE);
	pass(&mut app, source, 3, UP_TO_DATE, 30);
	assert_eq!(request(&mut app, b, 3, UP_TO_DATE), Some((30, UP_TO_DATE)));
	assert_eq!(request(&mut app, waiting, 3, UP_TO_DATE), Some((30, UP_TO_DATE)));
	mark_dirty(&mut app, 3, 4, None);
	assert_eq!(events(&app, a), vec!["pass 3 UpToDate 30", "dirty 3 4 None"]);
	assert_eq!(events(&app, b), vec!["dirty 3 4 None"]);
	// up to date data is asked for again, the dirtied data is enough for an older request
	assert_eq!(request(&mut app, a, 3, UP_TO_DATE), None);
	assert_eq!(producer(&app, source).requests, vec![(3, UP_TO_DATE), (3, UP_TO_DATE)]);
	assert_eq!(request(&mut app, b, 3, UP_TO_DATE), None);
	assert_eq!(producer(&app, source).requests.len(), 2);
	assert_eq!(request(&mut app, waiting, 3, dirty(4)), Some((30, dirty(4))));
	// a second dirty does not make it older
	mark_dirty(&mut app, 3, 6, None);
	assert_eq!(request(&mut app, waiting, 3, dirty(4)), Some((30, dirty(4))));
	assert_eq!(request(&mut app, waiting, 3, dirty(5)), None);
	assert_eq!(producer(&app, source).requests.len(), 3);
	// the dirty does not cancel anything
	assert!(producer(&app, source).cancels.is_empty());
	pass(&mut app, source, 3, UP_TO_DATE, 31);
	assert_eq!(events(&app, a).last(), Some(&"pass 3 UpToDate 31"));
	assert_eq!(events(&app, b).last(), Some(&"pass 3 UpToDate 31"));
	assert_eq!(events(&app, waiting).last(), Some(&"pass 3 UpToDate 31"));
}

#[test]
#[should_panic(expected = "No fulfiller of bile_data::tests::Sum")]
fn a_request_no_fulfiller_can_take_panics() {
	let mut app = sum_app();
	let a = app.world_mut().spawn(Consumer::default()).id();
	request(&mut app, a, 3, UP_TO_DATE);
}

// ============================ Cancelling ============================

#[test]
fn cancel_keeps_what_another_node_waits_for() {
	let mut app = sum_app();
	let source = app.world_mut().spawn(Producer::new(1.0)).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	let b = app.world_mut().spawn(Consumer::default()).id();
	let c = app.world_mut().spawn(Consumer::default()).id();
	request(&mut app, a, 3, UP_TO_DATE);
	request(&mut app, b, 3, UP_TO_DATE);
	assert_eq!(producer(&app, source).requests.len(), 1);
	cancel(&mut app, a, 3, UP_TO_DATE, None);
	assert!(producer(&app, source).cancels.is_empty());
	pass(&mut app, source, 3, UP_TO_DATE, 30);
	assert!(events(&app, a).is_empty());
	assert_eq!(events(&app, b), vec!["pass 3 UpToDate 30"]);
	// the last cancel throws the data away
	cancel(&mut app, b, 3, UP_TO_DATE, None);
	assert_eq!(producer(&app, source).cancels, vec![(3, UP_TO_DATE, None)]);
	assert_eq!(request(&mut app, c, 3, UP_TO_DATE), None);
	assert_eq!(producer(&app, source).requests.len(), 2);
}

#[test]
fn cancel_only_reaches_what_nobody_waits_for() {
	let mut app = sum_app();
	let source = app.world_mut().spawn(Producer::new(1.0)).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	let b = app.world_mut().spawn(Consumer::default()).id();
	request(&mut app, a, 3, UP_TO_DATE);
	request(&mut app, b, 3, dirty(5));
	assert_eq!(producer(&app, source).requests, vec![(3, UP_TO_DATE), (3, dirty(5))]);
	cancel(&mut app, a, 3, UP_TO_DATE, None);
	assert_eq!(producer(&app, source).cancels, vec![(3, UP_TO_DATE, Some(6))]);
	pass(&mut app, source, 3, dirty(5), 50);
	assert_eq!(events(&app, b), vec!["pass 3 Dirty { tick: 5 } 50"]);
	assert!(events(&app, a).is_empty());
	cancel(&mut app, b, 3, UP_TO_DATE, None);
	assert_eq!(producer(&app, source).cancels, vec![(3, UP_TO_DATE, Some(6)), (3, UP_TO_DATE, None)]);
}

#[test]
fn the_node_keeps_the_data_while_it_is_used_from_the_storage() {
	let mut app = sum_app();
	let source = app.world_mut().spawn(Producer::new(1.0)).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	let b = app.world_mut().spawn(Consumer::default()).id();
	request(&mut app, a, 3, UP_TO_DATE);
	pass(&mut app, source, 3, UP_TO_DATE, 30);
	mark_dirty(&mut app, 3, 3, None);
	// only a's request reached the node
	assert_eq!(request(&mut app, b, 3, dirty(2)), Some((30, dirty(3))));
	cancel(&mut app, a, 3, UP_TO_DATE, None);
	assert!(producer(&app, source).cancels.is_empty());
	cancel(&mut app, b, 3, UP_TO_DATE, None);
	assert_eq!(producer(&app, source).cancels, vec![(3, UP_TO_DATE, None)]);

	// also while the request of a is still in flight
	request(&mut app, a, 4, UP_TO_DATE);
	pass(&mut app, source, 4, dirty(3), 40);
	assert_eq!(request(&mut app, b, 4, dirty(2)), Some((40, dirty(3))));
	cancel(&mut app, a, 4, UP_TO_DATE, None);
	assert_eq!(producer(&app, source).cancels.len(), 1);
	cancel(&mut app, b, 4, UP_TO_DATE, None);
	assert_eq!(producer(&app, source).cancels[1], (4, UP_TO_DATE, None));
}

#[test]
fn a_cancel_releases_the_requests_one_pass_satisfied() {
	let mut app = sum_app();
	let source = app.world_mut().spawn(Producer::new(1.0)).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	request(&mut app, a, 3, dirty(5));
	request(&mut app, a, 3, UP_TO_DATE);
	pass(&mut app, source, 3, UP_TO_DATE, 30);
	cancel(&mut app, a, 3, UP_TO_DATE, Some(6));
	cancel(&mut app, a, 3, UP_TO_DATE, None);
	assert_eq!(producer(&app, source).cancels, vec![(3, UP_TO_DATE, None)]);
}

#[test]
fn a_cancel_keeps_the_data_another_node_uses() {
	let mut app = sum_app();
	let source = app.world_mut().spawn(Producer::new(1.0)).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	let b = app.world_mut().spawn(Consumer::default()).id();
	request(&mut app, a, 3, dirty(5));
	request(&mut app, b, 3, UP_TO_DATE);
	pass(&mut app, source, 3, UP_TO_DATE, 30);
	cancel(&mut app, b, 3, UP_TO_DATE, None);
	assert!(producer(&app, source).cancels.is_empty());
	cancel(&mut app, a, 3, UP_TO_DATE, None);
	assert_eq!(producer(&app, source).cancels, vec![(3, UP_TO_DATE, None)]);
}

#[test]
fn a_cancel_that_is_not_passed_on_keeps_the_request_in_flight() {
	let mut app = sum_app();
	let source = app.world_mut().spawn(Producer::new(1.0)).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	let b = app.world_mut().spawn(Consumer::default()).id();
	request(&mut app, a, 3, UP_TO_DATE);
	pass(&mut app, source, 3, UP_TO_DATE, 30);
	mark_dirty(&mut app, 3, 5, None);
	assert_eq!(request(&mut app, a, 3, dirty(5)), Some((30, dirty(5))));
	assert_eq!(request(&mut app, b, 3, UP_TO_DATE), None);
	// the node is not told, a cancel of UpToDate would also cover what its data is used for
	cancel(&mut app, b, 3, UP_TO_DATE, None);
	assert!(producer(&app, source).cancels.is_empty());
	// so it still works on it
	assert_eq!(request(&mut app, b, 3, UP_TO_DATE), None);
	assert_eq!(producer(&app, source).requests.len(), 2);
}

#[test]
fn a_cancel_keeps_the_node_whose_data_is_used() {
	let mut app = sum_app();
	let first = app.world_mut().spawn(Producer::new(1.0)).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	let b = app.world_mut().spawn(Consumer::default()).id();
	request(&mut app, a, 3, dirty(5));
	// the next requests go to another node
	let second = app.world_mut().spawn(Producer::new(0.5)).id();
	request(&mut app, b, 3, UP_TO_DATE);
	pass(&mut app, first, 3, UP_TO_DATE, 30);
	mark_dirty(&mut app, 3, 5, None);
	assert_eq!(request(&mut app, a, 3, dirty(1)), Some((30, dirty(5))));
	assert_eq!(request(&mut app, b, 3, dirty(6)), None);
	cancel(&mut app, b, 3, dirty(6), Some(5));
	assert_eq!(producer(&app, second).cancels, vec![(3, dirty(6), Some(5))]);
	assert!(producer(&app, first).cancels.is_empty());
}

#[derive(Resource)]
struct OtherRequester { entity: Entity, passes: Vec<u32> }
impl RequesterAPI<Sum> for OtherRequester {
	fn pass_data(&mut self, _requester_params: &mut DynRequesterParams<Sum>, _params: &u32, _dirty_state: DirtyState, data: &u32) { self.passes.push(*data); }
	fn dirty_data(&mut self, _requester_params: &mut DynRequesterParams<Sum>, _params: &u32, _last_clean_tick: u64, _diff: Option<&u32>) {}
}

fn other_request(app: &mut App, cancel: bool, dirty_state: DirtyState) -> Option<(u32, DirtyState)> {
	app.world_mut()
		.run_system_once(move |
			mut storage: Single<One<&mut dyn StorageRequesterAPI<Sum>>, With<IsResource>>,
			mut storage_params: DynStorageRequesterParams<Sum>,
			mut requester: Query<&mut OtherRequester>,
		| {
			let entity = requester.single_mut().unwrap().entity;
			if cancel {
				storage.cancel(&mut storage_params, entity, &3, dirty_state, None);
				return None;
			}
			storage.request(&mut storage_params, entity, &Request::new(3, dirty_state))
		})
		.unwrap()
}

#[test]
fn requesters_do_not_cancel_each_other() {
	let mut app = sum_app();
	app.insert_resource(OtherRequester { entity: Entity::PLACEHOLDER, passes: Vec::new() });
	let entity = app.world().resource_entity::<OtherRequester>().unwrap();
	app.world_mut().resource_mut::<OtherRequester>().entity = entity;
	app.register_component_as::<dyn RequesterAPI<Sum>, OtherRequester>();
	let source = app.world_mut().spawn(Producer::new(1.0)).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	assert_eq!(other_request(&mut app, false, UP_TO_DATE), None);
	request(&mut app, a, 3, UP_TO_DATE);
	assert_eq!(producer(&app, source).requests.len(), 1);
	// it gets nothing it no longer asks for
	other_request(&mut app, true, UP_TO_DATE);
	assert!(producer(&app, source).cancels.is_empty());
	pass(&mut app, source, 3, UP_TO_DATE, 30);
	assert_eq!(events(&app, a), vec!["pass 3 UpToDate 30"]);
	assert!(app.world().resource::<OtherRequester>().passes.is_empty());
	assert_eq!(other_request(&mut app, false, UP_TO_DATE), Some((30, UP_TO_DATE)));
	cancel(&mut app, a, 3, UP_TO_DATE, None);
	assert!(producer(&app, source).cancels.is_empty());
	other_request(&mut app, true, UP_TO_DATE);
	assert_eq!(producer(&app, source).cancels, vec![(3, UP_TO_DATE, None)]);
}

#[test]
fn a_pass_reaches_only_the_requesters_that_wait() {
	let mut app = sum_app();
	app.insert_resource(OtherRequester { entity: Entity::PLACEHOLDER, passes: Vec::new() });
	let entity = app.world().resource_entity::<OtherRequester>().unwrap();
	app.world_mut().resource_mut::<OtherRequester>().entity = entity;
	app.register_component_as::<dyn RequesterAPI<Sum>, OtherRequester>();
	let source = app.world_mut().spawn(Producer::new(1.0)).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	request(&mut app, a, 3, UP_TO_DATE);
	other_request(&mut app, false, UP_TO_DATE);
	pass(&mut app, source, 3, UP_TO_DATE, 30);
	assert_eq!(events(&app, a), vec!["pass 3 UpToDate 30"]);
	assert_eq!(app.world().resource::<OtherRequester>().passes, vec![30]);
	mark_dirty(&mut app, 3, 4, None);
	request(&mut app, a, 3, UP_TO_DATE);
	pass(&mut app, source, 3, UP_TO_DATE, 31);
	assert_eq!(events(&app, a).last(), Some(&"pass 3 UpToDate 31"));
	assert_eq!(app.world().resource::<OtherRequester>().passes, vec![30]);
}

#[derive(Resource)]
struct OtherFulfiller { cost: f32, cancels: Vec<(DirtyState, Option<u64>)> }
impl FulfillerAPI<Sum> for OtherFulfiller {
	fn cost(&self, _fulfiller_params: &mut DynFulfillerParams<Sum>, _request: &Request<Sum>) -> Option<f32> { Some(self.cost) }
	fn request(&mut self, _fulfiller_params: &mut DynFulfillerParams<Sum>, _request: Request<Sum>) {}
	fn cancel(&mut self, _fulfiller_params: &mut DynFulfillerParams<Sum>, _params: &u32, latest: DirtyState, oldest: Option<u64>) { self.cancels.push((latest, oldest)); }
}

fn other_pass(app: &mut App, fulfiller: Entity, dirty_state: DirtyState, data: u32) {
	app.world_mut()
		.run_system_once(move |mut storage: Single<One<&mut dyn StorageFulfillerAPI<Sum>>, With<IsResource>>, mut storage_params: DynStorageFulfillerParams<Sum>| {
			storage.pass_data(&mut storage_params, fulfiller, &3, dirty_state, data);
		})
		.unwrap();
}

#[test]
fn a_cancel_reaches_only_the_fulfillers_it_covers() {
	let mut app = sum_app();
	app.insert_resource(OtherRequester { entity: Entity::PLACEHOLDER, passes: Vec::new() });
	let entity = app.world().resource_entity::<OtherRequester>().unwrap();
	app.world_mut().resource_mut::<OtherRequester>().entity = entity;
	app.register_component_as::<dyn RequesterAPI<Sum>, OtherRequester>();
	app.insert_resource(OtherFulfiller { cost: 2.0, cancels: Vec::new() });
	app.register_component_as::<dyn FulfillerAPI<Sum>, OtherFulfiller>();
	let other = app.world().resource_entity::<OtherFulfiller>().unwrap();
	let source = app.world_mut().spawn(Producer::new(1.0)).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	// a keeps the data of the node
	request(&mut app, a, 3, dirty(10));
	pass(&mut app, source, 3, UP_TO_DATE, 30);
	mark_dirty(&mut app, 3, 20, None);
	// the other requester gets newer data from the other fulfiller
	app.world_mut().resource_mut::<OtherFulfiller>().cost = 0.5;
	other_request(&mut app, false, UP_TO_DATE);
	other_pass(&mut app, other, UP_TO_DATE, 31);
	mark_dirty(&mut app, 3, 21, None);
	assert_eq!(other_request(&mut app, false, dirty(22)), None);
	other_request(&mut app, true, dirty(22));
	assert_eq!(app.world().resource::<OtherFulfiller>().cancels, vec![(dirty(22), None)]);
	assert!(producer(&app, source).cancels.is_empty());
}

#[test]
fn a_cancel_keeps_the_fulfiller_whose_data_is_used() {
	let mut app = sum_app();
	app.insert_resource(OtherRequester { entity: Entity::PLACEHOLDER, passes: Vec::new() });
	let entity = app.world().resource_entity::<OtherRequester>().unwrap();
	app.world_mut().resource_mut::<OtherRequester>().entity = entity;
	app.register_component_as::<dyn RequesterAPI<Sum>, OtherRequester>();
	app.insert_resource(OtherFulfiller { cost: 2.0, cancels: Vec::new() });
	app.register_component_as::<dyn FulfillerAPI<Sum>, OtherFulfiller>();
	let source = app.world_mut().spawn(Producer::new(1.0)).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	request(&mut app, a, 3, UP_TO_DATE);
	// the other requester asks the other fulfiller, and gets the data of the node
	app.world_mut().resource_mut::<OtherFulfiller>().cost = 0.5;
	other_request(&mut app, false, dirty(5));
	other_request(&mut app, false, dirty(6));
	pass(&mut app, source, 3, dirty(5), 50);
	assert_eq!(app.world().resource::<OtherRequester>().passes, vec![50]);
	cancel(&mut app, a, 3, UP_TO_DATE, None);
	assert!(producer(&app, source).cancels.is_empty());
}

// ============================ Diffs ============================

fn diff_app(adder: Adder) -> (App, Entity, Entity, Entity) {
	let mut app = sum_app();
	app.register_diff_applier::<Sum, Adder>();
	let source = app.world_mut().spawn(Producer::new(1.0)).id();
	let adder = app.world_mut().spawn(adder).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	request(&mut app, a, 3, UP_TO_DATE);
	pass(&mut app, source, 3, UP_TO_DATE, 30);
	mark_dirty(&mut app, 3, 1, Some(5));
	(app, source, adder, a)
}

#[test]
fn diffs_are_applied_instead_of_producing_again() {
	let (mut app, source, _, a) = diff_app(Adder { cost: 0.5, applies: true });
	assert_eq!(events(&app, a), vec!["pass 3 UpToDate 30", "dirty 3 1 Some(5)"]);
	assert_eq!(request(&mut app, a, 3, UP_TO_DATE), Some((35, UP_TO_DATE)));
	mark_dirty(&mut app, 3, 2, Some(1));
	mark_dirty(&mut app, 3, 2, Some(2));
	assert_eq!(request(&mut app, a, 3, UP_TO_DATE), Some((38, UP_TO_DATE)));
	assert_eq!(producer(&app, source).requests.len(), 1);
	// the diffs before a dirty without a diff are applied for a request they satisfy
	mark_dirty(&mut app, 3, 3, Some(1));
	mark_dirty(&mut app, 3, 4, None);
	mark_dirty(&mut app, 3, 5, Some(1));
	assert_eq!(request(&mut app, a, 3, dirty(4)), Some((39, dirty(4))));
	assert_eq!(request(&mut app, a, 3, UP_TO_DATE), None);
	assert_eq!(producer(&app, source).requests, vec![(3, UP_TO_DATE), (3, UP_TO_DATE)]);
	// up to date data has every dirty
	pass(&mut app, source, 3, UP_TO_DATE, 100);
	assert_eq!(events(&app, a).last(), Some(&"pass 3 UpToDate 100"));
	mark_dirty(&mut app, 3, 6, Some(5));
	assert_eq!(request(&mut app, a, 3, UP_TO_DATE), Some((105, UP_TO_DATE)));
	assert_eq!(producer(&app, source).requests.len(), 2);
}

#[test]
fn diffs_are_only_applied_when_that_is_cheaper() {
	let (mut app, source, _, a) = diff_app(Adder { cost: 2.0, applies: true });
	assert_eq!(request(&mut app, a, 3, UP_TO_DATE), None);
	assert_eq!(producer(&app, source).requests.len(), 2);
}

#[test]
fn data_that_arrives_dirty_keeps_the_diffs_it_lacks() {
	let (mut app, source, adder, a) = diff_app(Adder { cost: 2.0, applies: true });
	mark_dirty(&mut app, 3, 2, Some(1));
	assert_eq!(request(&mut app, a, 3, dirty(2)), None);
	// the data has the first diff but not the second
	pass(&mut app, source, 3, dirty(2), 35);
	assert_eq!(events(&app, a).last(), Some(&"pass 3 Dirty { tick: 2 } 35"));
	app.world_mut().get_mut::<Adder>(adder).unwrap().cost = 0.5;
	assert_eq!(request(&mut app, a, 3, UP_TO_DATE), Some((36, UP_TO_DATE)));
	assert_eq!(producer(&app, source).requests.len(), 2);
}

#[test]
fn diffs_can_be_applied_later() {
	let (mut app, source, adder, a) = diff_app(Adder { cost: 0.5, applies: false });
	assert_eq!(request(&mut app, a, 3, dirty(2)), None);
	assert_eq!(producer(&app, source).requests.len(), 1);
	// a dirty arrives while it works
	mark_dirty(&mut app, 3, 2, Some(1));
	pass_diffed(&mut app, adder, 3, 35);
	assert_eq!(events(&app, a).last(), Some(&"pass 3 Dirty { tick: 2 } 35"));
	// it was asked once
	pass_diffed(&mut app, adder, 3, 99);
	app.world_mut().get_mut::<Adder>(adder).unwrap().applies = true;
	assert_eq!(request(&mut app, a, 3, UP_TO_DATE), Some((36, UP_TO_DATE)));
	assert_eq!(producer(&app, source).requests.len(), 1);
}

#[test]
fn a_request_the_diffed_data_is_too_old_for_is_made_again() {
	let (mut app, source, adder, a) = diff_app(Adder { cost: 0.5, applies: false });
	assert_eq!(request(&mut app, a, 3, UP_TO_DATE), None);
	// a dirty arrives while it works, so its data is not up to date
	mark_dirty(&mut app, 3, 2, Some(1));
	pass_diffed(&mut app, adder, 3, 35);
	assert_eq!(events(&app, a).last(), Some(&"dirty 3 2 Some(1)"));
	app.world_mut().get_mut::<Adder>(adder).unwrap().applies = true;
	app.update();
	assert_eq!(events(&app, a).last(), Some(&"pass 3 UpToDate 36"));
	assert_eq!(producer(&app, source).requests.len(), 1);
}

#[test]
fn a_late_dirty_is_kept_while_a_diff_applier_works() {
	let (mut app, source, adder, a) = diff_app(Adder { cost: 0.5, applies: false });
	mark_dirty(&mut app, 3, 10, Some(2));
	assert_eq!(request(&mut app, a, 3, UP_TO_DATE), None);
	// the dirty of an earlier tick arrives late, the data it passes lacks it
	mark_dirty(&mut app, 3, 5, Some(1));
	pass_diffed(&mut app, adder, 3, 37);
	assert_eq!(events(&app, a).last(), Some(&"dirty 3 5 Some(1)"));
	// its diff can not be applied after the diff of tick 10, so the node is asked
	app.world_mut().get_mut::<Adder>(adder).unwrap().applies = true;
	app.update();
	assert_eq!(events(&app, a).last(), Some(&"dirty 3 5 Some(1)"));
	assert_eq!(producer(&app, source).requests.len(), 2);
}

#[test]
fn older_data_leaves_the_diff_applier_working() {
	let (mut app, source, adder, a) = diff_app(Adder { cost: 0.5, applies: false });
	assert_eq!(request(&mut app, a, 3, UP_TO_DATE), None);
	// the node passes data older than the stored data
	pass(&mut app, source, 3, dirty(0), 29);
	app.update();
	assert_eq!(producer(&app, source).requests.len(), 1);
	pass_diffed(&mut app, adder, 3, 35);
	assert_eq!(events(&app, a).last(), Some(&"pass 3 UpToDate 35"));
}

#[test]
fn newer_data_drops_what_the_diff_applier_passes() {
	let (mut app, source, adder, a) = diff_app(Adder { cost: 0.5, applies: false });
	let b = app.world_mut().spawn(Consumer::default()).id();
	assert_eq!(request(&mut app, a, 3, UP_TO_DATE), None);
	mark_dirty(&mut app, 3, 2, Some(1));
	assert_eq!(request(&mut app, b, 3, dirty(2)), None);
	// newer data arrives while it works, what it passes is based on the older data
	pass(&mut app, source, 3, dirty(2), 40);
	pass_diffed(&mut app, adder, 3, 35);
	assert_eq!(request(&mut app, b, 3, dirty(2)), Some((40, dirty(2))));
	// what the newer data is not enough for is requested again
	app.world_mut().get_mut::<Adder>(adder).unwrap().applies = true;
	app.update();
	assert_eq!(events(&app, a).last(), Some(&"pass 3 UpToDate 41"));
	assert_eq!(producer(&app, source).requests.len(), 2);
}

#[test]
fn a_pass_keeps_the_dirty_state_it_came_with() {
	let (mut app, source, adder, a) = diff_app(Adder { cost: 2.0, applies: true });
	let b = app.world_mut().spawn(Consumer::default()).id();
	assert_eq!(request(&mut app, a, 3, UP_TO_DATE), None);
	// the node knows of a dirty at tick 5 the storage did not get yet
	pass(&mut app, source, 3, dirty(5), 40);
	assert_eq!(events(&app, a).last(), Some(&"dirty 3 1 Some(5)"));
	assert_eq!(request(&mut app, b, 3, dirty(5)), Some((40, dirty(5))));
	// so no diff is applied past it
	app.world_mut().get_mut::<Adder>(adder).unwrap().cost = 0.5;
	mark_dirty(&mut app, 3, 7, Some(2));
	assert_eq!(request(&mut app, b, 3, dirty(7)), None);
	assert_eq!(producer(&app, source).requests.len(), 3);
}

#[test]
fn a_cancelled_diff_applier_is_not_asked_again_until_it_passed() {
	let (mut app, source, adder, a) = diff_app(Adder { cost: 0.5, applies: false });
	assert_eq!(request(&mut app, a, 3, dirty(2)), None);
	cancel(&mut app, a, 3, dirty(2), Some(2));
	mark_dirty(&mut app, 3, 2, Some(1));
	// it still works on what was cancelled, so the node is asked
	assert_eq!(request(&mut app, a, 3, UP_TO_DATE), None);
	assert_eq!(producer(&app, source).requests.len(), 2);
	// and what it passes is not taken for the new request
	pass_diffed(&mut app, adder, 3, 35);
	assert_eq!(events(&app, a).last(), Some(&"dirty 3 2 Some(1)"));
}

#[test]
fn a_request_is_not_made_again_if_the_stored_data_is_enough() {
	let (mut app, source, adder, a) = diff_app(Adder { cost: 0.5, applies: false });
	let b = app.world_mut().spawn(Consumer::default()).id();
	assert_eq!(request(&mut app, a, 3, UP_TO_DATE), None);
	mark_dirty(&mut app, 3, 2, Some(1));
	pass_diffed(&mut app, adder, 3, 35);
	// another node has the rest applied before the request is made again
	app.world_mut().get_mut::<Adder>(adder).unwrap().applies = true;
	assert_eq!(request(&mut app, b, 3, UP_TO_DATE), Some((36, UP_TO_DATE)));
	app.update();
	assert_eq!(events(&app, a).last(), Some(&"pass 3 UpToDate 36"));
	assert_eq!(producer(&app, source).requests.len(), 1);
}

#[test]
fn a_late_dirty_the_data_has_does_not_dirty_it() {
	let mut app = sum_app();
	let source = app.world_mut().spawn(Producer::new(1.0)).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	request(&mut app, a, 3, dirty(5));
	// it has every dirty before tick 5
	pass(&mut app, source, 3, dirty(5), 40);
	mark_dirty(&mut app, 3, 3, Some(3));
	assert_eq!(request(&mut app, a, 3, dirty(5)), Some((40, dirty(5))));
	assert_eq!(producer(&app, source).requests.len(), 1);
}

#[test]
fn a_late_diff_is_not_applied_after_newer_ones() {
	let (mut app, source, _, a) = diff_app(Adder { cost: 0.5, applies: true });
	assert_eq!(request(&mut app, a, 3, UP_TO_DATE), Some((35, UP_TO_DATE)));
	// from before the diff that was applied
	mark_dirty(&mut app, 3, 0, Some(1));
	assert_eq!(request(&mut app, a, 3, UP_TO_DATE), None);
	assert_eq!(producer(&app, source).requests.len(), 2);
}

#[test]
fn applied_diffs_reach_the_nodes_that_wait() {
	let (mut app, source, adder, a) = diff_app(Adder { cost: 2.0, applies: true });
	let b = app.world_mut().spawn(Consumer::default()).id();
	assert_eq!(request(&mut app, b, 3, dirty(2)), None);
	assert_eq!(request(&mut app, a, 3, dirty(2)), None);
	assert_eq!(producer(&app, source).requests.len(), 2);
	app.world_mut().get_mut::<Adder>(adder).unwrap().cost = 0.5;
	assert_eq!(request(&mut app, a, 3, UP_TO_DATE), Some((35, UP_TO_DATE)));
	app.update();
	assert_eq!(events(&app, b), vec!["pass 3 UpToDate 35"]);
	// a got it already
	assert_eq!(events(&app, a), vec!["pass 3 UpToDate 30", "dirty 3 1 Some(5)"]);
}

#[test]
fn a_dirty_after_applied_diffs_keeps_its_diff() {
	let (mut app, source, _, a) = diff_app(Adder { cost: 0.5, applies: true });
	assert_eq!(request(&mut app, a, 3, UP_TO_DATE), Some((35, UP_TO_DATE)));
	mark_dirty(&mut app, 3, 2, Some(1));
	app.update();
	assert_eq!(request(&mut app, a, 3, UP_TO_DATE), Some((36, UP_TO_DATE)));
	assert_eq!(producer(&app, source).requests.len(), 1);
}

// Like a source that can also edit its data
#[derive(Component)]
struct Both(Producer, Adder);
impl ProducingNodeAPI<Sum> for Both {
	fn cost(&self, request: &Request<Sum>) -> Option<f32> { self.0.cost(request) }
	fn request(&mut self, request: Request<Sum>) { self.0.request(request); }
	fn cancel(&mut self, params: &u32, latest: DirtyState, oldest: Option<u64>) { self.0.cancel(params, latest, oldest); }
}
impl DiffApplyingNode<Sum> for Both {
	fn diff_cost(&self, request: &Request<Sum>, diffs: &[u32]) -> Option<f32> { self.1.diff_cost(request, diffs) }
	fn apply(&mut self, params: &u32, data: &mut u32, diffs: Vec<u32>) -> bool { self.1.apply(params, data, diffs) }
}

#[test]
fn a_node_can_produce_and_apply_diffs() {
	let mut app = sum_app();
	app.register_producer_simple::<Sum, Both>().register_diff_applier::<Sum, Both>();
	let both = app.world_mut().spawn(Both(Producer::new(1.0), Adder { cost: 0.5, applies: true })).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	request(&mut app, a, 3, UP_TO_DATE);
	pass(&mut app, both, 3, UP_TO_DATE, 30);
	mark_dirty(&mut app, 3, 1, Some(5));
	assert_eq!(request(&mut app, a, 3, UP_TO_DATE), Some((35, UP_TO_DATE)));
	assert_eq!(app.world().get::<Both>(both).unwrap().0.requests.len(), 1);
}

// ============================ Schedule ============================

#[test]
fn state_tick_advances_once_per_schedule_run() {
	let mut app = sum_app();
	let tick = app.world().resource::<StateTick>().current_tick();
	app.update();
	assert_eq!(app.world().resource::<StateTick>().current_tick(), tick + 1);
	app.world_mut().run_schedule(BileDataSchedule);
	assert_eq!(app.world().resource::<StateTick>().current_tick(), tick + 2);
	let current_tick = app.world_mut().run_system_once(|dirtier: DataDirtier<Sum>| dirtier.current_tick()).unwrap();
	assert_eq!(current_tick, tick + 2);
}
