use bevy::ecs::system::RunSystemOnce;

use crate::{diff::BileRegisterDiff, gather::BileRegisterGather, setup::{DataType, Request}, simple::{self, BileRegisterSimple}, streaming::{self, BileRegisterStreaming}, *};

struct Parts;
impl DataType for Parts {
	type Params = u32;
	type Data = u32;
	type Diff = ();
}

#[derive(Component, Default)]
struct Consumer(Vec<String>);
impl streaming::DataConsumingNode<Parts> for Consumer {
	fn start_data(&mut self, params: &u32, _tick: u64) { self.0.push(format!("start {params}")); }
	fn push_data(&mut self, params: &u32, data: u32) { self.0.push(format!("push {params} {data}")); }
	fn finish_data(&mut self, params: &u32) { self.0.push(format!("finish {params}")); }
	fn dirty_data(&mut self, params: &u32, _tick: u64, _diff: Option<&()>) { self.0.push(format!("dirty {params}")); }
}

#[derive(Component, Default)]
struct StreamProducer(Vec<(u32, streaming::StreamId)>, Vec<u32>, Option<f32>);
impl streaming::DataProducingNode<Parts> for StreamProducer {
	fn cost(&self, _request: &Request<Parts>) -> Option<f32> { self.2 }
	fn request(&mut self, request: Request<Parts>, stream_id: streaming::StreamId) { self.0.push((request.params, stream_id)); }
	fn drop(&mut self, params: &u32) { self.1.push(*params); }
}

#[derive(Component, Default)]
struct GatherProducer(Vec<(u32, streaming::StreamId)>, Vec<u32>, Option<f32>);
impl gather::DataProducingNode<Parts> for GatherProducer {
	fn cost(&self, _request: &Request<Parts>) -> Option<f32> { self.2 }
	fn request(&mut self, request: Request<Parts>, stream_id: streaming::StreamId) { self.0.push((request.params, stream_id)); }
	fn drop(&mut self, params: &u32) { self.1.push(*params); }
}

fn request_parts(app: &mut App, entity: Entity) {
	app.world_mut()
		.run_system_once(move |mut requester: streaming::DataRequester<Parts>| requester.request(Request { params: 3, allowed_tick: DirtyState::UpToDate }, entity))
		.unwrap();
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

#[test]
fn gather_does_not_depend_on_the_registration_order() {
	let mut app = App::new();
	app.add_plugins(BileDataPlugin);
	app.register_consumer_streaming::<Parts, Consumer>().register_producer_gather::<Parts, GatherProducer>();
	let x = app.world_mut().spawn(GatherProducer(vec![], vec![], Some(1.0))).id();
	let y = app.world_mut().spawn(GatherProducer(vec![], vec![], Some(2.0))).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	request_parts(&mut app, a);
	assert_eq!(app.world().get::<GatherProducer>(x).unwrap().0.len(), 1);
	assert_eq!(app.world().get::<GatherProducer>(y).unwrap().0.len(), 1);
}

#[test]
fn the_cheapest_fulfiller_streams() {
	let mut app = App::new();
	app.add_plugins(BileDataPlugin);
	app.register_producer_streaming::<Parts, StreamProducer>()
		.register_producer_gather::<Parts, GatherProducer>()
		.register_consumer_streaming::<Parts, Consumer>();
	let producer = app.world_mut().spawn(StreamProducer(vec![], vec![], Some(2.0))).id();
	let x = app.world_mut().spawn(GatherProducer(vec![], vec![], Some(1.0))).id();
	let y = app.world_mut().spawn(GatherProducer(vec![], vec![], Some(1.5))).id();
	let a = app.world_mut().spawn(Consumer::default()).id();
	// the gather costs as much as its most expensive node
	request_parts(&mut app, a);
	assert!(app.world().get::<StreamProducer>(producer).unwrap().0.is_empty());
	let (_, id) = app.world().get::<GatherProducer>(x).unwrap().0[0];
	assert_eq!(app.world().get::<GatherProducer>(y).unwrap().0, vec![(3, id)]);
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.start(x, &3, id, DirtyState::UpToDate);
			passer.start(y, &3, id, DirtyState::UpToDate);
			passer.push(x, &3, id, 10);
			passer.finish(x, &3, id);
			passer.finish(y, &3, id);
		})
		.unwrap();
	app.update();
	assert_eq!(events(&app, a), vec!["start 3", "push 3 10", "finish 3"]);
	// the gather got more expensive, so the stream producer streams the new data and the gather nodes are dropped
	app.world_mut().run_system_once(|mut dirtier: streaming::DataDirtier<Parts>| dirtier.mark_dirty(&3, None)).unwrap();
	app.world_mut().get_mut::<GatherProducer>(y).unwrap().2 = Some(5.0);
	request_parts(&mut app, a);
	assert_eq!(app.world().get::<GatherProducer>(x).unwrap().0.len(), 1);
	let (_, id) = app.world().get::<StreamProducer>(producer).unwrap().0[0];
	app.world_mut()
		.run_system_once(move |mut passer: streaming::DataPasser<Parts>| {
			passer.start(producer, &3, id, DirtyState::UpToDate);
			passer.push(producer, &3, id, 20);
			passer.finish(producer, &3, id);
		})
		.unwrap();
	app.update();
	assert_eq!(events(&app, a), vec!["start 3", "push 3 10", "finish 3", "dirty 3", "start 3", "push 3 20", "finish 3"]);
	assert_eq!(app.world().get::<GatherProducer>(x).unwrap().1, vec![3]);
	assert_eq!(app.world().get::<GatherProducer>(y).unwrap().1, vec![3]);
	assert!(app.world().get::<StreamProducer>(producer).unwrap().1.is_empty());
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
