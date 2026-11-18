use bevy::ecs::system::RunSystemOnce;

use crate::{diff::BileRegisterDiff, gather::BileRegisterGather, simple::{self, BileRegisterSimple}, streaming::{self, BileRegisterStreaming, StreamingDataType}, *};

struct Parts;
impl StreamingDataType for Parts {
	type Params = u32;
	type Data = u32;
	type Diff = u32;
}

#[derive(Component, Default)]
struct Consumer(Vec<String>);
impl streaming::DataConsumingNode<Parts> for Consumer {
	fn start_data(&mut self, params: &u32, _tick: u64) { self.0.push(format!("start {params}")); }
	fn push_data(&mut self, params: &u32, data: u32) { self.0.push(format!("push {params} {data}")); }
	fn finish_data(&mut self, params: &u32) { self.0.push(format!("finish {params}")); }
	fn dirty_data(&mut self, params: &u32, _tick: u64, diff: Option<&u32>) { self.0.push(diff.map_or(format!("dirty {params}"), |diff| format!("dirty {params} {diff}"))); }
}

#[derive(Component, Default)]
struct StreamProducer(Vec<(u32, streaming::StreamId)>, Vec<u32>, Option<f32>);
impl streaming::DataProducingNode<Parts> for StreamProducer {
	fn cost(&self, _request: &streaming::Request<Parts>) -> Option<f32> { self.2 }
	fn request(&mut self, request: streaming::Request<Parts>, stream_id: streaming::StreamId) { self.0.push((request.params, stream_id)); }
	fn drop(&mut self, params: &u32) { self.1.push(*params); }
}

#[derive(Component, Default)]
struct GatherProducer(Vec<(u32, streaming::StreamId)>, Vec<u32>, Option<f32>);
impl gather::DataProducingNode<Parts> for GatherProducer {
	fn cost(&self, _request: &streaming::Request<Parts>) -> Option<f32> { self.2 }
	fn request(&mut self, request: streaming::Request<Parts>, stream_id: streaming::StreamId) { self.0.push((request.params, stream_id)); }
	fn drop(&mut self, params: &u32) { self.1.push(*params); }
}

fn request_parts(app: &mut App, entity: Entity) {
	app.world_mut()
		.run_system_once(move |mut requester: streaming::DataRequester<Parts>| requester.request(streaming::Request { params: 3, allowed_tick: DirtyState::UpToDate }, entity))
		.unwrap();
}

fn cancel_parts(app: &mut App, entity: Entity) {
	app.world_mut().run_system_once(move |mut requester: streaming::DataRequester<Parts>| requester.cancel(&3, entity)).unwrap();
}

fn dirty_parts(app: &mut App, diff: Option<u32>) {
	app.world_mut().run_system_once(move |mut dirtier: streaming::DataDirtier<Parts>| dirtier.mark_dirty(&3, diff.as_ref())).unwrap();
}

fn events(app: &App, entity: Entity) -> Vec<&str> {
	app.world().get::<Consumer>(entity).unwrap().0.iter().map(String::as_str).collect()
}

#[test]
fn streaming_producer_streams() {
	let mut app = App::new();
	app.add_plugins(BileDataPlugin);
	app.register_producer_streaming::<Parts, StreamProducer>().register_consumer_streaming::<Parts, Consumer>();
	let producer = app.world_mut().spawn(StreamProducer(vec![], vec![], Some(1.0))).id();
	app.world_mut().spawn(StreamProducer(vec![], vec![], Some(2.0)));
	let a = app.world_mut().spawn(Consumer::default()).id();
	let b = app.world_mut().spawn(Consumer::default()).id();
	let c = app.world_mut().spawn(Consumer::default()).id();
	request_parts(&mut app, a);
	let (params, id) = app.world().get::<StreamProducer>(producer).unwrap().0[0];
	assert_eq!(params, 3);
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.start(producer, &3, id, DirtyState::UpToDate);
			passer.push(producer, &3, id, 10);
		})
		.unwrap();
	app.update();
	assert_eq!(events(&app, a), vec!["start 3", "push 3 10"]);
	// a requester that is late gets what was already streamed and then the rest
	request_parts(&mut app, b);
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.push(producer, &3, id, 20);
			passer.finish(producer, &3, id);
		})
		.unwrap();
	app.update();
	assert_eq!(events(&app, a), vec!["start 3", "push 3 10", "push 3 20", "finish 3"]);
	assert_eq!(events(&app, b), vec!["start 3", "push 3 10", "push 3 20", "finish 3"]);
	// also after the stream finished
	request_parts(&mut app, c);
	app.update();
	assert_eq!(events(&app, c), vec!["start 3", "push 3 10", "push 3 20", "finish 3"]);
	assert_eq!(app.world().get::<StreamProducer>(producer).unwrap().0.len(), 1);
	app.world_mut().run_system_once(|mut dirtier: streaming::DataDirtier<Parts>| dirtier.mark_dirty(&3, None)).unwrap();
	assert_eq!(*events(&app, a).last().unwrap(), "dirty 3");
	// dirty data is not replayed
	request_parts(&mut app, a);
	assert_eq!(app.world().get::<StreamProducer>(producer).unwrap().0.len(), 2);
}

#[test]
fn gather_requests_every_node() {
	let mut app = App::new();
	app.add_plugins(BileDataPlugin);
	app.register_producer_gather::<Parts, GatherProducer>().register_consumer_streaming::<Parts, Consumer>();
	let x = app.world_mut().spawn(GatherProducer(vec![], vec![], Some(1.0))).id();
	let y = app.world_mut().spawn(GatherProducer(vec![], vec![], Some(2.0))).id();
	let z = app.world_mut().spawn(GatherProducer(vec![], vec![], None)).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	let b = app.world_mut().spawn(Consumer::default()).id();
	request_parts(&mut app, a);
	let (_, id) = app.world().get::<GatherProducer>(x).unwrap().0[0];
	assert_eq!(app.world().get::<GatherProducer>(y).unwrap().0, vec![(3, id)]);
	assert!(app.world().get::<GatherProducer>(z).unwrap().0.is_empty());
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.start(x, &3, id, DirtyState::UpToDate);
			passer.push(x, &3, id, 10);
			passer.finish(x, &3, id);
			passer.push(x, &3, id, 90);
		})
		.unwrap();
	app.update();
	assert_eq!(events(&app, a), vec!["start 3", "push 3 10"]);
	request_parts(&mut app, b);
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.start(y, &3, id, DirtyState::UpToDate);
			passer.push(y, &3, id, 20);
		})
		.unwrap();
	app.world_mut().run_system_once(move |mut requester: streaming::DataRequester<Parts>| { requester.cancel(&3, a); requester.cancel(&3, b); }).unwrap();
	app.update();
	assert_eq!(events(&app, a), vec!["start 3", "push 3 10", "push 3 20"]);
	assert_eq!(events(&app, b), vec!["start 3", "push 3 10", "push 3 20"]);
	assert_eq!(app.world().get::<GatherProducer>(x).unwrap().1, vec![3]);
	assert_eq!(app.world().get::<GatherProducer>(y).unwrap().1, vec![3]);
	assert!(app.world().get::<GatherProducer>(z).unwrap().1.is_empty());
}

struct Sum;
impl DataType for Sum {
	type Params = u32;
	type Data = u32;
	type Diff = u32;
}

#[derive(Component, Default)]
struct SumProducer(Vec<u32>, Vec<u32>);
impl simple::DataProducingNode<Sum> for SumProducer {
	fn cost(&self, _request: &Request<Sum>) -> Option<f32> { Some(1.0) }
	fn request(&mut self, request: Request<Sum>) { self.0.push(request.params); }
	fn drop(&mut self, params: &u32) { self.1.push(*params); }
}

#[derive(Component, Default)]
struct Adder { instant: bool, pending: Option<u32>, drops: Vec<u32> }
impl simple::DataProducingNode<Sum> for Adder {
	fn cost(&self, _request: &Request<Sum>) -> Option<f32> { None }
	fn request(&mut self, _request: Request<Sum>) { unreachable!() }
	fn drop(&mut self, params: &u32) { self.drops.push(*params); }
}
impl diff::DiffApplyingNode<Sum> for Adder {
	fn diff_cost(&self, _request: &Request<Sum>, _diffs: &[u32]) -> Option<f32> { Some(0.5) }
	fn apply(&mut self, _params: &u32, data: &mut u32, diffs: Vec<u32>) -> bool {
		let sum = *data + diffs.iter().sum::<u32>();
		if self.instant { *data = sum; } else { self.pending = Some(sum); }
		self.instant
	}
}

#[derive(Component, Default)]
struct SumConsumer(Vec<String>);
impl simple::DataConsumingNode<Sum> for SumConsumer {
	fn pass_data(&mut self, _params: &u32, _tick: u64, data: u32) { self.0.push(format!("pass {data}")); }
	fn dirty_data(&mut self, _params: &u32, _tick: u64, diff: Option<&u32>) { self.0.push(format!("dirty {diff:?}")); }
}

fn request_sum(app: &mut App, entity: Entity) -> Option<u32> {
	app.world_mut().run_system_once(move |mut requester: simple::DataRequester<Sum>| requester.request(Request::new(3, DirtyState::UpToDate), entity)).unwrap()
}
fn pass_sum(app: &mut App, source: Entity, data: u32) {
	app.world_mut().run_system_once(move |mut passer: simple::DataPasser<Sum>| passer.pass_data(source, &3, DirtyState::UpToDate, data)).unwrap();
}
fn dirty_sum(app: &mut App, diff: Option<u32>) {
	app.world_mut().run_system_once(move |mut dirtier: simple::DataDirtier<Sum>| dirtier.mark_dirty(&3, diff.as_ref())).unwrap();
}
fn sum_app(instant: bool) -> (App, Entity, Entity, Entity) {
	let mut app = App::new();
	app.add_plugins(BileDataPlugin);
	app.register_producer_simple::<Sum, SumProducer>().register_consumer_simple::<Sum, SumConsumer>().register_diff_applier::<Sum, Adder>();
	let producer = app.world_mut().spawn(SumProducer::default()).id();
	let adder = app.world_mut().spawn(Adder { instant, ..default() }).id();
	let consumer = app.world_mut().spawn(SumConsumer::default()).id();
	app.update();
	assert_eq!(request_sum(&mut app, consumer), None);
	pass_sum(&mut app, producer, 30);
	app.update();
	dirty_sum(&mut app, Some(5));
	(app, producer, adder, consumer)
}

#[test]
fn diffs_are_applied_instead_of_producing_again() {
	let (mut app, producer, adder, consumer) = sum_app(true);
	assert_eq!(app.world().get::<SumConsumer>(consumer).unwrap().0, vec!["pass 30", "dirty Some(5)"]);
	assert_eq!(request_sum(&mut app, consumer), Some(35));
	dirty_sum(&mut app, Some(1));
	dirty_sum(&mut app, Some(2));
	assert_eq!(request_sum(&mut app, consumer), Some(38));
	app.update();
	assert_eq!(app.world().get::<SumProducer>(producer).unwrap().0, vec![3]);
	assert!(app.world().get::<SumProducer>(producer).unwrap().1.is_empty());
	// a dirty without a diff ends the run
	dirty_sum(&mut app, Some(1));
	dirty_sum(&mut app, None);
	dirty_sum(&mut app, Some(1));
	assert_eq!(request_sum(&mut app, consumer), None);
	assert_eq!(app.world().get::<SumProducer>(producer).unwrap().0, vec![3, 3]);
	pass_sum(&mut app, producer, 100);
	app.update();
	assert_eq!(app.world().get::<SumConsumer>(consumer).unwrap().0.last().unwrap(), "pass 100");
	assert_eq!(app.world().get::<Adder>(adder).unwrap().drops, vec![3]);
	assert!(app.world().get::<SumProducer>(producer).unwrap().1.is_empty());
	app.world_mut().run_system_once(move |mut requester: simple::DataRequester<Sum>| requester.cancel(&3, consumer)).unwrap();
	app.update();
	assert_eq!(app.world().get::<SumProducer>(producer).unwrap().1, vec![3]);
}

#[test]
fn diffs_can_be_applied_later() {
	let (mut app, producer, adder, consumer) = sum_app(false);
	assert_eq!(request_sum(&mut app, consumer), None);
	assert_eq!(app.world().get::<Adder>(adder).unwrap().pending, Some(35));
	dirty_sum(&mut app, Some(7)); // not in the data the adder is working on
	pass_sum(&mut app, adder, 35);
	app.update();
	assert_eq!(app.world().get::<SumProducer>(producer).unwrap().0, vec![3]);
	assert!(app.world().get::<SumProducer>(producer).unwrap().1.is_empty());
	assert_eq!(request_sum(&mut app, consumer), None);
	assert_eq!(app.world().get::<Adder>(adder).unwrap().pending, Some(42));
	pass_sum(&mut app, adder, 42);
	app.update();
	assert_eq!(app.world().get::<SumConsumer>(consumer).unwrap().0.last().unwrap(), "pass 42");
	assert_eq!(request_sum(&mut app, consumer), Some(42));
	app.world_mut().run_system_once(move |mut requester: simple::DataRequester<Sum>| requester.cancel(&3, consumer)).unwrap();
	app.update();
	assert_eq!(app.world().get::<SumProducer>(producer).unwrap().1, vec![3]);
	assert_eq!(app.world().get::<Adder>(adder).unwrap().drops, vec![3]);
}

#[test]
fn dirtying_updates_the_state_tick() {
	let (mut app, ..) = sum_app(true);
	let tick = app.world().resource::<StateTick>().current_tick();
	dirty_sum(&mut app, None);
	dirty_sum(&mut app, None);
	assert_eq!(app.world().resource::<StateTick>().current_tick(), tick + 2);
}

#[test]
fn finished_stream_is_released_when_its_last_user_cancels() {
	let mut app = App::new();
	app.add_plugins(BileDataPlugin);
	app.register_producer_streaming::<Parts, StreamProducer>().register_consumer_streaming::<Parts, Consumer>();
	let producer = app.world_mut().spawn(StreamProducer(vec![], vec![], Some(1.0))).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	let b = app.world_mut().spawn(Consumer::default()).id();
	request_parts(&mut app, a);
	let (_, id) = app.world().get::<StreamProducer>(producer).unwrap().0[0];
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.start(producer, &3, id, DirtyState::UpToDate);
			passer.push(producer, &3, id, 10);
			passer.finish(producer, &3, id);
		})
		.unwrap();
	request_parts(&mut app, b);
	app.update();
	assert_eq!(events(&app, a), vec!["start 3", "push 3 10", "finish 3"]);
	assert_eq!(events(&app, b), vec!["start 3", "push 3 10", "finish 3"]);
	// the finished stream stays for as long as it has a user
	app.world_mut().run_system_once(move |mut requester: streaming::DataRequester<Parts>| requester.cancel(&3, a)).unwrap();
	app.update();
	assert!(app.world().get::<StreamProducer>(producer).unwrap().1.is_empty());
	request_parts(&mut app, a);
	app.update();
	assert_eq!(events(&app, a), vec!["start 3", "push 3 10", "finish 3", "start 3", "push 3 10", "finish 3"]);
	assert_eq!(app.world().get::<StreamProducer>(producer).unwrap().0.len(), 1);
	// the last cancel releases it and drops the producer
	app.world_mut().run_system_once(move |mut requester: streaming::DataRequester<Parts>| { requester.cancel(&3, a); requester.cancel(&3, b); }).unwrap();
	app.update();
	assert_eq!(app.world().get::<StreamProducer>(producer).unwrap().1, vec![3]);
	// so nothing is left to replay and the producer is requested again
	request_parts(&mut app, a);
	app.update();
	assert_eq!(events(&app, a).len(), 6);
	let requests = &app.world().get::<StreamProducer>(producer).unwrap().0;
	assert_eq!(requests.len(), 2);
	assert_ne!(requests[1].1, id);
}

#[test]
fn finished_gather_is_released_when_its_last_user_cancels() {
	let mut app = App::new();
	app.add_plugins(BileDataPlugin);
	app.register_producer_gather::<Parts, GatherProducer>().register_consumer_streaming::<Parts, Consumer>();
	let x = app.world_mut().spawn(GatherProducer(vec![], vec![], Some(1.0))).id();
	let y = app.world_mut().spawn(GatherProducer(vec![], vec![], Some(2.0))).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	request_parts(&mut app, a);
	let (_, id) = app.world().get::<GatherProducer>(x).unwrap().0[0];
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.start(x, &3, id, DirtyState::UpToDate);
			passer.start(y, &3, id, DirtyState::UpToDate);
			passer.push(x, &3, id, 10);
			passer.push(y, &3, id, 20);
			passer.finish(x, &3, id);
			passer.finish(y, &3, id);
		})
		.unwrap();
	app.update();
	assert_eq!(events(&app, a), vec!["start 3", "push 3 10", "push 3 20", "finish 3"]);
	assert!(app.world().get::<GatherProducer>(x).unwrap().1.is_empty());
	assert!(app.world().get::<GatherProducer>(y).unwrap().1.is_empty());
	// the last cancel releases the finished stream and drops every node of it
	app.world_mut().run_system_once(move |mut requester: streaming::DataRequester<Parts>| requester.cancel(&3, a)).unwrap();
	app.update();
	assert_eq!(app.world().get::<GatherProducer>(x).unwrap().1, vec![3]);
	assert_eq!(app.world().get::<GatherProducer>(y).unwrap().1, vec![3]);
	// so nothing is left to replay and every node is requested again
	request_parts(&mut app, a);
	app.update();
	assert_eq!(events(&app, a).len(), 4);
	let requests = &app.world().get::<GatherProducer>(x).unwrap().0;
	assert_eq!(requests.len(), 2);
	assert_ne!(requests[1].1, id);
	assert_eq!(app.world().get::<GatherProducer>(y).unwrap().0, *requests);
}

#[test]
fn gather_replacing_a_live_stream_does_not_drop_its_nodes() {
	let mut app = App::new();
	app.add_plugins(BileDataPlugin);
	app.register_producer_gather::<Parts, GatherProducer>().register_consumer_streaming::<Parts, Consumer>();
	let x = app.world_mut().spawn(GatherProducer(vec![], vec![], Some(1.0))).id();
	let y = app.world_mut().spawn(GatherProducer(vec![], vec![], Some(2.0))).id();
	let z = app.world_mut().spawn(GatherProducer(vec![], vec![], Some(3.0))).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	request_parts(&mut app, a);
	let (_, old_id) = app.world().get::<GatherProducer>(x).unwrap().0[0];
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.start(x, &3, old_id, DirtyState::UpToDate);
			passer.push(x, &3, old_id, 10);
		})
		.unwrap();
	app.update();
	assert_eq!(events(&app, a), vec!["start 3", "push 3 10"]);
	// the data is dirtied while y and z are still producing it and is requested again, z has nothing for it anymore
	app.world_mut().run_system_once(|mut dirtier: streaming::DataDirtier<Parts>| dirtier.mark_dirty(&3, None)).unwrap();
	app.world_mut().get_mut::<GatherProducer>(z).unwrap().2 = None;
	request_parts(&mut app, a);
	let (_, id) = app.world().get::<GatherProducer>(x).unwrap().0[1];
	assert_ne!(id, old_id);
	assert_eq!(app.world().get::<GatherProducer>(y).unwrap().0, vec![(3, old_id), (3, id)]);
	assert_eq!(app.world().get::<GatherProducer>(z).unwrap().0, vec![(3, old_id)]);
	app.world_mut().run_system_once(move |mut passer: streaming::DataPasser<Parts>| passer.start(x, &3, id, DirtyState::UpToDate)).unwrap();
	app.update();
	// x and y produce the new stream now, a drop would stop that too; only z is no longer needed
	assert!(app.world().get::<GatherProducer>(x).unwrap().1.is_empty());
	assert!(app.world().get::<GatherProducer>(y).unwrap().1.is_empty());
	assert_eq!(app.world().get::<GatherProducer>(z).unwrap().1, vec![3]);
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.push(x, &3, id, 11);
			passer.finish(x, &3, id);
			passer.push(y, &3, old_id, 20); // of the replaced stream
			passer.start(y, &3, id, DirtyState::UpToDate);
			passer.push(y, &3, id, 21);
			passer.finish(y, &3, id);
		})
		.unwrap();
	app.update();
	assert_eq!(events(&app, a), vec!["start 3", "push 3 10", "dirty 3", "start 3", "push 3 11", "push 3 21", "finish 3"]);
	assert!(app.world().get::<GatherProducer>(x).unwrap().1.is_empty());
	assert!(app.world().get::<GatherProducer>(y).unwrap().1.is_empty());
}

#[test]
fn gather_cancelled_before_any_start_drops_the_requested_nodes() {
	let mut app = App::new();
	app.add_plugins(BileDataPlugin);
	app.register_producer_gather::<Parts, GatherProducer>().register_consumer_streaming::<Parts, Consumer>();
	let x = app.world_mut().spawn(GatherProducer(vec![], vec![], Some(1.0))).id();
	let y = app.world_mut().spawn(GatherProducer(vec![], vec![], Some(2.0))).id();
	let z = app.world_mut().spawn(GatherProducer(vec![], vec![], None)).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	request_parts(&mut app, a);
	let (_, old_id) = app.world().get::<GatherProducer>(x).unwrap().0[0];
	assert_eq!(app.world().get::<GatherProducer>(y).unwrap().0, vec![(3, old_id)]);
	// the only requester cancels while no node has started yet, the nodes that were asked are told to stop
	app.world_mut().run_system_once(move |mut requester: streaming::DataRequester<Parts>| requester.cancel(&3, a)).unwrap();
	app.update();
	assert_eq!(app.world().get::<GatherProducer>(x).unwrap().1, vec![3]);
	assert_eq!(app.world().get::<GatherProducer>(y).unwrap().1, vec![3]);
	assert!(app.world().get::<GatherProducer>(z).unwrap().1.is_empty());
	// a new request reaches them again with a new stream, which works
	request_parts(&mut app, a);
	let requests = app.world().get::<GatherProducer>(x).unwrap().0.clone();
	assert_eq!(requests.len(), 2);
	let id = requests[1].1;
	assert_ne!(id, old_id);
	assert_eq!(app.world().get::<GatherProducer>(y).unwrap().0, requests);
	assert!(app.world().get::<GatherProducer>(z).unwrap().0.is_empty());
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.start(y, &3, old_id, DirtyState::UpToDate); // of the cancelled request
			passer.start(x, &3, id, DirtyState::UpToDate);
			passer.start(y, &3, id, DirtyState::UpToDate);
			passer.push(x, &3, id, 10);
			passer.push(y, &3, id, 20);
			passer.finish(x, &3, id);
			passer.finish(y, &3, id);
		})
		.unwrap();
	app.update();
	assert_eq!(events(&app, a), vec!["start 3", "push 3 10", "push 3 20", "finish 3"]);
	assert_eq!(app.world().get::<GatherProducer>(x).unwrap().1, vec![3]);
	assert_eq!(app.world().get::<GatherProducer>(y).unwrap().1, vec![3]);
}

#[test]
fn gather_cancelled_before_the_replacing_stream_starts_drops_its_nodes_too() {
	let mut app = App::new();
	app.add_plugins(BileDataPlugin);
	app.register_producer_gather::<Parts, GatherProducer>().register_consumer_streaming::<Parts, Consumer>();
	let x = app.world_mut().spawn(GatherProducer(vec![], vec![], Some(1.0))).id();
	let y = app.world_mut().spawn(GatherProducer(vec![], vec![], None)).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	request_parts(&mut app, a);
	let (_, old_id) = app.world().get::<GatherProducer>(x).unwrap().0[0];
	assert!(app.world().get::<GatherProducer>(y).unwrap().0.is_empty());
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.start(x, &3, old_id, DirtyState::UpToDate);
			passer.push(x, &3, old_id, 10);
		})
		.unwrap();
	app.update();
	assert_eq!(events(&app, a), vec!["start 3", "push 3 10"]);
	// the data is dirtied and requested again, now y has something for it too
	app.world_mut().run_system_once(|mut dirtier: streaming::DataDirtier<Parts>| dirtier.mark_dirty(&3, None)).unwrap();
	app.world_mut().get_mut::<GatherProducer>(y).unwrap().2 = Some(2.0);
	request_parts(&mut app, a);
	let (_, id) = app.world().get::<GatherProducer>(x).unwrap().0[1];
	assert_ne!(id, old_id);
	assert_eq!(app.world().get::<GatherProducer>(y).unwrap().0, vec![(3, id)]);
	// the only requester cancels before the new stream starts: x is a node of the old stream, y only of the new request
	app.world_mut().run_system_once(move |mut requester: streaming::DataRequester<Parts>| requester.cancel(&3, a)).unwrap();
	app.update();
	assert_eq!(app.world().get::<GatherProducer>(x).unwrap().1, vec![3]);
	assert_eq!(app.world().get::<GatherProducer>(y).unwrap().1, vec![3]);
	// nothing of the cancelled request is accepted afterwards
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.start(y, &3, id, DirtyState::UpToDate);
			passer.push(y, &3, id, 20);
		})
		.unwrap();
	app.update();
	assert_eq!(events(&app, a), vec!["start 3", "push 3 10", "dirty 3"]);
	assert_eq!(app.world().get::<GatherProducer>(y).unwrap().1, vec![3]);
}

#[test]
fn replaced_stream_data_keeps_its_users() {
	let mut app = App::new();
	app.add_plugins(BileDataPlugin);
	app.register_producer_streaming::<Parts, StreamProducer>().register_consumer_streaming::<Parts, Consumer>();
	let producer = app.world_mut().spawn(StreamProducer(vec![], vec![], Some(1.0))).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	let b = app.world_mut().spawn(Consumer::default()).id();
	request_parts(&mut app, a);
	let (_, old_id) = app.world().get::<StreamProducer>(producer).unwrap().0[0];
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.start(producer, &3, old_id, DirtyState::UpToDate);
			passer.push(producer, &3, old_id, 10);
			passer.finish(producer, &3, old_id);
		})
		.unwrap();
	app.update();
	dirty_parts(&mut app, Some(7));
	assert_eq!(events(&app, a), vec!["start 3", "push 3 10", "finish 3", "dirty 3 7"]);
	// b requests the dirty data, a keeps using what it has
	request_parts(&mut app, b);
	let (_, id) = app.world().get::<StreamProducer>(producer).unwrap().0[1];
	assert_ne!(id, old_id);
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.start(producer, &3, id, DirtyState::UpToDate);
			passer.push(producer, &3, id, 11);
			passer.finish(producer, &3, id);
		})
		.unwrap();
	app.update();
	// the new stream is only passed to b, who asked for it
	assert_eq!(events(&app, b), vec!["start 3", "push 3 11", "finish 3"]);
	assert_eq!(events(&app, a), vec!["start 3", "push 3 10", "finish 3", "dirty 3 7"]);
	// a is still a user of the data, so it is told when it is dirtied again
	dirty_parts(&mut app, Some(8));
	assert_eq!(*events(&app, a).last().unwrap(), "dirty 3 8");
	assert_eq!(*events(&app, b).last().unwrap(), "dirty 3 8");
	// and the data is not dropped under it when b cancels
	cancel_parts(&mut app, b);
	app.update();
	assert!(app.world().get::<StreamProducer>(producer).unwrap().1.is_empty());
	dirty_parts(&mut app, None);
	assert_eq!(*events(&app, a).last().unwrap(), "dirty 3");
	assert_eq!(*events(&app, b).last().unwrap(), "dirty 3 8");
	cancel_parts(&mut app, a);
	app.update();
	assert_eq!(app.world().get::<StreamProducer>(producer).unwrap().1, vec![3]);
}

#[test]
fn replaced_gather_data_keeps_its_users() {
	let mut app = App::new();
	app.add_plugins(BileDataPlugin);
	app.register_producer_gather::<Parts, GatherProducer>().register_consumer_streaming::<Parts, Consumer>();
	let x = app.world_mut().spawn(GatherProducer(vec![], vec![], Some(1.0))).id();
	let y = app.world_mut().spawn(GatherProducer(vec![], vec![], Some(2.0))).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	let b = app.world_mut().spawn(Consumer::default()).id();
	request_parts(&mut app, a);
	let (_, old_id) = app.world().get::<GatherProducer>(x).unwrap().0[0];
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.start(x, &3, old_id, DirtyState::UpToDate);
			passer.start(y, &3, old_id, DirtyState::UpToDate);
			passer.push(x, &3, old_id, 10);
			passer.push(y, &3, old_id, 20);
			passer.finish(x, &3, old_id);
			passer.finish(y, &3, old_id);
		})
		.unwrap();
	app.update();
	dirty_parts(&mut app, Some(7));
	assert_eq!(events(&app, a), vec!["start 3", "push 3 10", "push 3 20", "finish 3", "dirty 3 7"]);
	// b requests the dirty data, a keeps using what it has
	request_parts(&mut app, b);
	let (_, id) = app.world().get::<GatherProducer>(x).unwrap().0[1];
	assert_ne!(id, old_id);
	assert_eq!(app.world().get::<GatherProducer>(y).unwrap().0, vec![(3, old_id), (3, id)]);
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.start(x, &3, id, DirtyState::UpToDate);
			passer.start(y, &3, id, DirtyState::UpToDate);
			passer.push(x, &3, id, 11);
			passer.push(y, &3, id, 21);
			passer.finish(x, &3, id);
			passer.finish(y, &3, id);
		})
		.unwrap();
	app.update();
	// the new stream is only passed to b, who asked for it
	assert_eq!(events(&app, b), vec!["start 3", "push 3 11", "push 3 21", "finish 3"]);
	assert_eq!(events(&app, a), vec!["start 3", "push 3 10", "push 3 20", "finish 3", "dirty 3 7"]);
	// a is still a user of the data, so it is told when it is dirtied again
	dirty_parts(&mut app, Some(8));
	assert_eq!(*events(&app, a).last().unwrap(), "dirty 3 8");
	assert_eq!(*events(&app, b).last().unwrap(), "dirty 3 8");
	// and the data is not dropped under it when b cancels
	cancel_parts(&mut app, b);
	app.update();
	assert!(app.world().get::<GatherProducer>(x).unwrap().1.is_empty());
	assert!(app.world().get::<GatherProducer>(y).unwrap().1.is_empty());
	dirty_parts(&mut app, None);
	assert_eq!(*events(&app, a).last().unwrap(), "dirty 3");
	assert_eq!(*events(&app, b).last().unwrap(), "dirty 3 8");
	cancel_parts(&mut app, a);
	app.update();
	assert_eq!(app.world().get::<GatherProducer>(x).unwrap().1, vec![3]);
	assert_eq!(app.world().get::<GatherProducer>(y).unwrap().1, vec![3]);
}

#[test]
fn stream_diffs_are_stored_until_the_data_is_replaced() {
	let mut app = App::new();
	app.add_plugins(BileDataPlugin);
	app.register_producer_streaming::<Parts, StreamProducer>().register_consumer_streaming::<Parts, Consumer>();
	let producer = app.world_mut().spawn(StreamProducer(vec![], vec![], Some(1.0))).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	let info = |app: &App| {
		let info = app.world().resource::<streaming::DataColumn<Parts>>().get(&3).unwrap();
		(info.data.clone(), info.diffs().map(<[u32]>::to_vec))
	};
	request_parts(&mut app, a);
	let (_, old_id) = app.world().get::<StreamProducer>(producer).unwrap().0[0];
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.start(producer, &3, old_id, DirtyState::UpToDate);
			passer.push(producer, &3, old_id, 10);
		})
		.unwrap();
	assert_eq!(info(&app), (vec![10], Some(vec![])));
	dirty_parts(&mut app, Some(5));
	dirty_parts(&mut app, Some(6));
	assert_eq!(info(&app), (vec![10], Some(vec![5, 6])));
	// they stay with the dirty data until the new data comes
	request_parts(&mut app, a);
	let (_, id) = app.world().get::<StreamProducer>(producer).unwrap().0[1];
	assert_eq!(info(&app), (vec![10], Some(vec![5, 6])));
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.start(producer, &3, id, DirtyState::UpToDate);
			passer.push(producer, &3, id, 11);
		})
		.unwrap();
	assert_eq!(info(&app), (vec![11], Some(vec![])));
	// a dirty without a diff ends the run
	dirty_parts(&mut app, Some(1));
	dirty_parts(&mut app, None);
	dirty_parts(&mut app, Some(2));
	assert_eq!(info(&app), (vec![11], None));
	request_parts(&mut app, a);
	let (_, id) = app.world().get::<StreamProducer>(producer).unwrap().0[2];
	app.world_mut().run_system_once(move |mut passer: streaming::DataPasser<Parts>| passer.start(producer, &3, id, DirtyState::UpToDate)).unwrap();
	assert_eq!(info(&app), (vec![], Some(vec![])));
}

#[test]
fn gather_diffs_are_stored_until_the_data_is_replaced() {
	let mut app = App::new();
	app.add_plugins(BileDataPlugin);
	app.register_producer_gather::<Parts, GatherProducer>().register_consumer_streaming::<Parts, Consumer>();
	let x = app.world_mut().spawn(GatherProducer(vec![], vec![], Some(1.0))).id();
	let y = app.world_mut().spawn(GatherProducer(vec![], vec![], Some(2.0))).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	let info = |app: &App| {
		let info = app.world().resource::<gather::DataColumn<Parts>>().get(&3).unwrap();
		(info.data.clone(), info.diffs().map(<[u32]>::to_vec))
	};
	request_parts(&mut app, a);
	let (_, old_id) = app.world().get::<GatherProducer>(x).unwrap().0[0];
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.start(x, &3, old_id, DirtyState::UpToDate);
			passer.push(x, &3, old_id, 10);
		})
		.unwrap();
	assert_eq!(info(&app), (vec![10], Some(vec![])));
	dirty_parts(&mut app, Some(5));
	// a node that joins later does not change what was dirtied
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.start(y, &3, old_id, DirtyState::UpToDate);
			passer.push(y, &3, old_id, 20);
		})
		.unwrap();
	dirty_parts(&mut app, Some(6));
	assert_eq!(info(&app), (vec![10, 20], Some(vec![5, 6])));
	// they stay with the dirty data until the new data comes
	request_parts(&mut app, a);
	let (_, id) = app.world().get::<GatherProducer>(x).unwrap().0[1];
	assert_eq!(info(&app), (vec![10, 20], Some(vec![5, 6])));
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.start(x, &3, id, DirtyState::UpToDate);
			passer.push(x, &3, id, 11);
			passer.start(y, &3, id, DirtyState::UpToDate);
			passer.push(y, &3, id, 21);
		})
		.unwrap();
	assert_eq!(info(&app), (vec![11, 21], Some(vec![])));
	// a dirty without a diff ends the run
	dirty_parts(&mut app, Some(1));
	dirty_parts(&mut app, None);
	dirty_parts(&mut app, Some(2));
	assert_eq!(info(&app), (vec![11, 21], None));
	request_parts(&mut app, a);
	let (_, id) = app.world().get::<GatherProducer>(x).unwrap().0[2];
	app.world_mut().run_system_once(move |mut passer: streaming::DataPasser<Parts>| passer.start(x, &3, id, DirtyState::UpToDate)).unwrap();
	assert_eq!(info(&app), (vec![], Some(vec![])));
	// the data is only as fresh as its least fresh part, there are no diffs for what a part lacks
	app.world_mut().run_system_once(move |mut passer: streaming::DataPasser<Parts>| passer.start(y, &3, id, DirtyState::Dirty { tick: 0 })).unwrap();
	assert_eq!(info(&app), (vec![], None));
}

#[test]
fn stream_request_cancelled_before_it_starts_keeps_the_node_of_used_data() {
	let mut app = App::new();
	app.add_plugins(BileDataPlugin);
	app.register_producer_streaming::<Parts, StreamProducer>().register_consumer_streaming::<Parts, Consumer>();
	let p = app.world_mut().spawn(StreamProducer(vec![], vec![], Some(1.0))).id();
	let q = app.world_mut().spawn(StreamProducer(vec![], vec![], Some(2.0))).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	let b = app.world_mut().spawn(Consumer::default()).id();
	request_parts(&mut app, a);
	let (_, old_id) = app.world().get::<StreamProducer>(p).unwrap().0[0];
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.start(p, &3, old_id, DirtyState::UpToDate);
			passer.push(p, &3, old_id, 10);
			passer.finish(p, &3, old_id);
		})
		.unwrap();
	app.update();
	dirty_parts(&mut app, None);
	// b requests the dirty data, for which q is the best node now, and cancels before q starts
	app.world_mut().get_mut::<StreamProducer>(q).unwrap().2 = Some(0.5);
	request_parts(&mut app, b);
	let (_, id) = app.world().get::<StreamProducer>(q).unwrap().0[0];
	assert_eq!(app.world().get::<StreamProducer>(p).unwrap().0.len(), 1);
	cancel_parts(&mut app, b);
	app.update();
	// q was only asked for that request and has nothing to work for anymore, the data a uses came from p
	assert!(app.world().get::<StreamProducer>(p).unwrap().1.is_empty());
	assert_eq!(app.world().get::<StreamProducer>(q).unwrap().1, vec![3]);
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.start(q, &3, id, DirtyState::UpToDate);
			passer.push(q, &3, id, 11);
		})
		.unwrap();
	app.update();
	assert!(app.world().get::<StreamProducer>(p).unwrap().1.is_empty());
	assert_eq!(app.world().get::<StreamProducer>(q).unwrap().1, vec![3]);
	// the same when p itself is asked again
	app.world_mut().get_mut::<StreamProducer>(q).unwrap().2 = None;
	request_parts(&mut app, b);
	let (_, id) = app.world().get::<StreamProducer>(p).unwrap().0[1];
	cancel_parts(&mut app, b);
	app.world_mut().run_system_once(move |mut passer: streaming::DataPasser<Parts>| passer.start(p, &3, id, DirtyState::UpToDate)).unwrap();
	app.update();
	assert!(app.world().get::<StreamProducer>(p).unwrap().1.is_empty());
	// a got nothing of the cancelled requests and is still a user of its data
	dirty_parts(&mut app, None);
	assert_eq!(events(&app, a), vec!["start 3", "push 3 10", "finish 3", "dirty 3", "dirty 3"]);
	cancel_parts(&mut app, a);
	app.update();
	assert_eq!(app.world().get::<StreamProducer>(p).unwrap().1, vec![3]);
	assert_eq!(app.world().get::<StreamProducer>(q).unwrap().1, vec![3]);
}

#[test]
fn gather_request_cancelled_before_it_starts_keeps_the_nodes_of_used_data() {
	let mut app = App::new();
	app.add_plugins(BileDataPlugin);
	app.register_producer_gather::<Parts, GatherProducer>().register_consumer_streaming::<Parts, Consumer>();
	let x = app.world_mut().spawn(GatherProducer(vec![], vec![], Some(1.0))).id();
	let y = app.world_mut().spawn(GatherProducer(vec![], vec![], Some(2.0))).id();
	let z = app.world_mut().spawn(GatherProducer(vec![], vec![], None)).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	let b = app.world_mut().spawn(Consumer::default()).id();
	request_parts(&mut app, a);
	let (_, old_id) = app.world().get::<GatherProducer>(x).unwrap().0[0];
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.start(x, &3, old_id, DirtyState::UpToDate);
			passer.start(y, &3, old_id, DirtyState::UpToDate);
			passer.push(x, &3, old_id, 10);
			passer.push(y, &3, old_id, 20);
			passer.finish(x, &3, old_id);
			passer.finish(y, &3, old_id);
		})
		.unwrap();
	app.update();
	dirty_parts(&mut app, None);
	// b requests the dirty data, for which z has something too now, and cancels before a node starts
	app.world_mut().get_mut::<GatherProducer>(z).unwrap().2 = Some(3.0);
	request_parts(&mut app, b);
	let (_, id) = app.world().get::<GatherProducer>(z).unwrap().0[0];
	assert_eq!(app.world().get::<GatherProducer>(x).unwrap().0, vec![(3, old_id), (3, id)]);
	assert_eq!(app.world().get::<GatherProducer>(y).unwrap().0, vec![(3, old_id), (3, id)]);
	cancel_parts(&mut app, b);
	app.update();
	assert!(app.world().get::<GatherProducer>(x).unwrap().1.is_empty());
	assert!(app.world().get::<GatherProducer>(y).unwrap().1.is_empty());
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.start(x, &3, id, DirtyState::UpToDate);
			passer.push(x, &3, id, 11);
			passer.start(y, &3, id, DirtyState::UpToDate);
		})
		.unwrap();
	app.update();
	// the data a uses came from x and y, so they are not dropped for starting a stream nobody waits for
	assert!(app.world().get::<GatherProducer>(x).unwrap().1.is_empty());
	assert!(app.world().get::<GatherProducer>(y).unwrap().1.is_empty());
	// z was only asked for that request and is dropped without having started
	assert_eq!(app.world().get::<GatherProducer>(z).unwrap().1, vec![3]);
	// a got nothing of the cancelled request and is still a user of its data
	dirty_parts(&mut app, None);
	assert_eq!(events(&app, a), vec!["start 3", "push 3 10", "push 3 20", "finish 3", "dirty 3", "dirty 3"]);
	cancel_parts(&mut app, a);
	app.update();
	assert_eq!(app.world().get::<GatherProducer>(x).unwrap().1, vec![3]);
	assert_eq!(app.world().get::<GatherProducer>(y).unwrap().1, vec![3]);
	assert_eq!(app.world().get::<GatherProducer>(z).unwrap().1, vec![3]);
}

#[test]
fn stale_stream_is_rejected_and_only_its_unused_node_is_dropped() {
	let mut app = App::new();
	app.add_plugins(BileDataPlugin);
	app.register_producer_streaming::<Parts, StreamProducer>().register_consumer_streaming::<Parts, Consumer>();
	let p = app.world_mut().spawn(StreamProducer(vec![], vec![], Some(1.0))).id();
	let q = app.world_mut().spawn(StreamProducer(vec![], vec![], Some(2.0))).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	request_parts(&mut app, a);
	let (_, old_id) = app.world().get::<StreamProducer>(p).unwrap().0[0];
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.start(p, &3, old_id, DirtyState::UpToDate);
			passer.push(p, &3, old_id, 10);
			passer.finish(p, &3, old_id);
		})
		.unwrap();
	app.update();
	dirty_parts(&mut app, None);
	dirty_parts(&mut app, None);
	// a requests the dirty data from q, whose stream is from before the data here was last clean
	app.world_mut().get_mut::<StreamProducer>(q).unwrap().2 = Some(0.5);
	request_parts(&mut app, a);
	let (_, id) = app.world().get::<StreamProducer>(q).unwrap().0[0];
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.start(q, &3, id, DirtyState::Dirty { tick: 0 });
			passer.push(q, &3, id, 11);
			passer.finish(q, &3, id);
		})
		.unwrap();
	app.update();
	assert_eq!(events(&app, a), vec!["start 3", "push 3 10", "finish 3", "dirty 3", "dirty 3"]);
	// q has nothing to work for anymore, the data a uses came from p
	assert!(app.world().get::<StreamProducer>(p).unwrap().1.is_empty());
	assert_eq!(app.world().get::<StreamProducer>(q).unwrap().1, vec![3]);
	cancel_parts(&mut app, a);
	app.update();
	assert_eq!(app.world().get::<StreamProducer>(p).unwrap().1, vec![3]);
	assert_eq!(app.world().get::<StreamProducer>(q).unwrap().1, vec![3]);
}

#[test]
fn stale_gather_is_rejected_and_only_its_unused_nodes_are_dropped() {
	let mut app = App::new();
	app.add_plugins(BileDataPlugin);
	app.register_producer_gather::<Parts, GatherProducer>().register_consumer_streaming::<Parts, Consumer>();
	let x = app.world_mut().spawn(GatherProducer(vec![], vec![], Some(1.0))).id();
	let y = app.world_mut().spawn(GatherProducer(vec![], vec![], None)).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	request_parts(&mut app, a);
	let (_, old_id) = app.world().get::<GatherProducer>(x).unwrap().0[0];
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.start(x, &3, old_id, DirtyState::UpToDate);
			passer.push(x, &3, old_id, 10);
			passer.finish(x, &3, old_id);
		})
		.unwrap();
	app.update();
	dirty_parts(&mut app, None);
	dirty_parts(&mut app, None);
	// a requests the dirty data from x and y, x starts a stream from before the data here was last clean
	app.world_mut().get_mut::<GatherProducer>(y).unwrap().2 = Some(2.0);
	request_parts(&mut app, a);
	let (_, id) = app.world().get::<GatherProducer>(y).unwrap().0[0];
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.start(x, &3, id, DirtyState::Dirty { tick: 0 });
			passer.push(x, &3, id, 11);
		})
		.unwrap();
	app.update();
	assert_eq!(events(&app, a), vec!["start 3", "push 3 10", "finish 3", "dirty 3", "dirty 3"]);
	// the stream is rejected as a whole: y is dropped without having started, the data a uses came from x
	assert!(app.world().get::<GatherProducer>(x).unwrap().1.is_empty());
	assert_eq!(app.world().get::<GatherProducer>(y).unwrap().1, vec![3]);
	app.world_mut().run_system_once(move |mut passer: streaming::DataPasser<Parts>| passer.start(y, &3, id, DirtyState::UpToDate)).unwrap();
	app.update();
	assert_eq!(events(&app, a).len(), 5);
	cancel_parts(&mut app, a);
	app.update();
	assert_eq!(app.world().get::<GatherProducer>(x).unwrap().1, vec![3]);
	assert_eq!(app.world().get::<GatherProducer>(y).unwrap().1, vec![3]);
}
