use bevy::ecs::system::RunSystemOnce;

use crate::{gather::BileRegisterGather, streaming_api::StreamId, *};
use crate::streaming_consuming::{BileRegisterStreamingConsumer, ConsumingNodeAPI, Requester};
use crate::streaming_producing::{BileRegisterStreamingProducer, DataDirtier, DataPasser, ProducingNodeAPI};
use crate::streaming_storage::BileAddStreamingStorage;

const UP_TO_DATE: DirtyState = DirtyState::UpToDate;
const fn dirty(tick: u64) -> DirtyState { DirtyState::Dirty { tick } }

// ============================ Nodes ============================

struct Parts;
impl DataType for Parts {
	type Params = u32;
	type Data = u32;
	type Diff = u32;
}

#[derive(Component)]
struct Producer { cost: Option<f32>, requests: Vec<(StreamId, u32, DirtyState)>, cancels: Vec<(u32, DirtyState, Option<u64>)> }
impl Producer {
	fn new(cost: Option<f32>) -> Self { Self { cost, requests: Vec::new(), cancels: Vec::new() } }
}
impl ProducingNodeAPI<Parts> for Producer {
	fn cost(&self, _request: &Request<Parts>) -> Option<f32> { self.cost }
	fn request(&mut self, stream_id: StreamId, request: Request<Parts>) { self.requests.push((stream_id, request.params, request.allowed_tick)); }
	fn cancel(&mut self, params: &u32, latest: DirtyState, oldest: Option<u64>) { self.cancels.push((*params, latest, oldest)); }
}

#[derive(Component, Default)]
struct Consumer(Vec<String>);
impl ConsumingNodeAPI<Parts> for Consumer {
	fn push_data(&mut self, _stream_id: StreamId, params: &u32, dirty_state: DirtyState, data: u32) { self.0.push(format!("push {params} {dirty_state:?} {data}")); }
	fn finish_data(&mut self, _stream_id: StreamId, params: &u32) { self.0.push(format!("finish {params}")); }
	fn dirty_data(&mut self, params: &u32, last_clean_tick: u64, diff: Option<&u32>) { self.0.push(format!("dirty {params} {last_clean_tick} {diff:?}")); }
}

fn streaming_app() -> App {
	let mut app = App::new();
	app.add_plugins(BileDataPlugin);
	app.add_streaming_storage::<Parts>().register_producer_streaming::<Parts, Producer>().register_consumer_streaming::<Parts, Consumer>();
	app
}
fn gather_app() -> App {
	let mut app = App::new();
	app.add_plugins(BileDataPlugin);
	app.add_streaming_storage::<Parts>().register_producer_gather::<Parts, Producer>().register_consumer_streaming::<Parts, Consumer>();
	app
}

fn request(app: &mut App, node: Entity, params: u32, allowed_tick: DirtyState) -> StreamId {
	app.world_mut().run_system_once(move |mut requester: Requester<Parts>| requester.request(Request::new(params, allowed_tick), node)).unwrap()
}
fn cancel(app: &mut App, node: Entity, params: u32, latest: DirtyState, oldest: Option<u64>) {
	app.world_mut().run_system_once(move |mut requester: Requester<Parts>| requester.cancel(&params, latest, oldest, node)).unwrap();
}
fn push(app: &mut App, source: Entity, stream_id: StreamId, params: u32, data: u32) {
	app.world_mut().run_system_once(move |mut passer: DataPasser<Parts>| passer.push(source, stream_id, &params, UP_TO_DATE, data)).unwrap();
}
fn finish(app: &mut App, source: Entity, stream_id: StreamId, params: u32) {
	app.world_mut().run_system_once(move |mut passer: DataPasser<Parts>| passer.finish(source, stream_id, &params)).unwrap();
}
fn mark_dirty(app: &mut App, params: u32, last_clean_tick: u64, diff: Option<u32>) {
	app.world_mut().run_system_once(move |mut dirtier: DataDirtier<Parts>| dirtier.mark_dirty(&params, last_clean_tick, diff.as_ref())).unwrap();
}
fn events(app: &App, node: Entity) -> Vec<&str> {
	app.world().get::<Consumer>(node).unwrap().0.iter().map(String::as_str).collect()
}
fn producer(app: &App, node: Entity) -> &Producer {
	app.world().get::<Producer>(node).unwrap()
}

// ============================ Streaming ============================

#[test]
fn the_cheapest_node_streams_to_the_consumer_in_order() {
	let mut app = streaming_app();
	let expensive = app.world_mut().spawn(Producer::new(Some(2.0))).id();
	let cheap = app.world_mut().spawn(Producer::new(Some(1.0))).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	let stream = request(&mut app, a, 3, UP_TO_DATE);
	assert_eq!(producer(&app, cheap).requests, vec![(stream, 3, UP_TO_DATE)]);
	assert!(producer(&app, expensive).requests.is_empty());
	// a node that was not asked passes nothing
	push(&mut app, expensive, stream, 3, 99);
	push(&mut app, cheap, stream, 3, 10);
	push(&mut app, cheap, stream, 3, 20);
	finish(&mut app, cheap, stream, 3);
	assert_eq!(events(&app, a), vec!["push 3 UpToDate 10", "push 3 UpToDate 20", "finish 3"]);
}

#[test]
fn a_request_joins_the_stream_until_it_pushed() {
	let mut app = streaming_app();
	let source = app.world_mut().spawn(Producer::new(Some(1.0))).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	let b = app.world_mut().spawn(Consumer::default()).id();
	let c = app.world_mut().spawn(Consumer::default()).id();
	let first = request(&mut app, a, 3, UP_TO_DATE);
	assert_eq!(request(&mut app, b, 3, UP_TO_DATE), first);
	assert_eq!(producer(&app, source).requests.len(), 1);
	push(&mut app, source, first, 3, 10);
	// nothing is kept, so a later request is a new stream
	let second = request(&mut app, c, 3, UP_TO_DATE);
	assert_ne!(second, first);
	assert_eq!(producer(&app, source).requests[1], (second, 3, UP_TO_DATE));
	push(&mut app, source, first, 3, 20);
	finish(&mut app, source, first, 3);
	push(&mut app, source, second, 3, 30);
	assert_eq!(events(&app, a), vec!["push 3 UpToDate 10", "push 3 UpToDate 20", "finish 3"]);
	assert_eq!(events(&app, b), events(&app, a));
	assert_eq!(events(&app, c), vec!["push 3 UpToDate 30"]);
}

#[test]
fn dirty_reaches_the_nodes_with_data_and_does_not_cancel() {
	let mut app = streaming_app();
	let source = app.world_mut().spawn(Producer::new(Some(1.0))).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	let b = app.world_mut().spawn(Consumer::default()).id();
	let first = request(&mut app, a, 3, UP_TO_DATE);
	push(&mut app, source, first, 3, 10);
	let second = request(&mut app, b, 3, UP_TO_DATE);
	mark_dirty(&mut app, 3, 4, Some(1));
	assert_eq!(events(&app, a), vec!["push 3 UpToDate 10", "dirty 3 4 Some(1)"]);
	assert!(events(&app, b).is_empty());
	assert!(producer(&app, source).cancels.is_empty());
	// a asks again and joins the stream that has not pushed yet
	assert_eq!(request(&mut app, a, 3, UP_TO_DATE), second);
	push(&mut app, source, second, 3, 11);
	assert_eq!(events(&app, a).last(), Some(&"push 3 UpToDate 11"));
	assert_eq!(events(&app, b), vec!["push 3 UpToDate 11"]);
}

#[test]
#[should_panic(expected = "No fulfiller of bile_data::streaming_tests::Parts")]
fn a_request_no_fulfiller_can_take_panics() {
	let mut app = streaming_app();
	let a = app.world_mut().spawn(Consumer::default()).id();
	request(&mut app, a, 3, UP_TO_DATE);
}

// ============================ Cancelling ============================

#[test]
fn cancel_keeps_what_another_node_holds() {
	let mut app = streaming_app();
	let source = app.world_mut().spawn(Producer::new(Some(1.0))).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	let b = app.world_mut().spawn(Consumer::default()).id();
	let stream = request(&mut app, a, 3, UP_TO_DATE);
	request(&mut app, b, 3, UP_TO_DATE);
	cancel(&mut app, a, 3, UP_TO_DATE, None);
	assert!(producer(&app, source).cancels.is_empty());
	push(&mut app, source, stream, 3, 10);
	assert!(events(&app, a).is_empty());
	assert_eq!(events(&app, b), vec!["push 3 UpToDate 10"]);
	// the last cancel reaches the node, midway through the stream
	cancel(&mut app, b, 3, UP_TO_DATE, None);
	assert_eq!(producer(&app, source).cancels, vec![(3, UP_TO_DATE, None)]);
	push(&mut app, source, stream, 3, 20);
	assert_eq!(events(&app, b).len(), 1);
}

#[test]
fn cancel_only_reaches_what_nobody_holds() {
	let mut app = streaming_app();
	let source = app.world_mut().spawn(Producer::new(Some(1.0))).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	let b = app.world_mut().spawn(Consumer::default()).id();
	request(&mut app, a, 3, UP_TO_DATE);
	let stream = request(&mut app, b, 3, dirty(5));
	assert_eq!(producer(&app, source).requests.len(), 2);
	cancel(&mut app, a, 3, UP_TO_DATE, None);
	assert_eq!(producer(&app, source).cancels, vec![(3, UP_TO_DATE, Some(6))]);
	push(&mut app, source, stream, 3, 50);
	assert_eq!(events(&app, b), vec!["push 3 UpToDate 50"]);
}

// ============================ Gather ============================

#[test]
fn gather_asks_every_node_that_can_and_finishes_when_all_did() {
	let mut app = gather_app();
	let x = app.world_mut().spawn(Producer::new(Some(1.0))).id();
	let y = app.world_mut().spawn(Producer::new(Some(2.0))).id();
	let z = app.world_mut().spawn(Producer::new(None)).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	let stream = request(&mut app, a, 3, UP_TO_DATE);
	assert_eq!(producer(&app, x).requests, vec![(stream, 3, UP_TO_DATE)]);
	assert_eq!(producer(&app, y).requests, vec![(stream, 3, UP_TO_DATE)]);
	assert!(producer(&app, z).requests.is_empty());
	push(&mut app, x, stream, 3, 10);
	finish(&mut app, x, stream, 3);
	// a node that finished or was not asked passes nothing
	push(&mut app, x, stream, 3, 90);
	push(&mut app, z, stream, 3, 99);
	assert_eq!(events(&app, a), vec!["push 3 UpToDate 10"]);
	push(&mut app, y, stream, 3, 20);
	finish(&mut app, y, stream, 3);
	assert_eq!(events(&app, a), vec!["push 3 UpToDate 10", "push 3 UpToDate 20", "finish 3"]);
	cancel(&mut app, a, 3, UP_TO_DATE, None);
	assert_eq!(producer(&app, x).cancels, vec![(3, UP_TO_DATE, None)]);
	assert_eq!(producer(&app, y).cancels, vec![(3, UP_TO_DATE, None)]);
	assert!(producer(&app, z).cancels.is_empty());
}
