use bevy::ecs::system::RunSystemOnce;
use bevy_trait_query::RegisterExt;

use crate::*;

struct Num;

impl DataType for Num {
	type Params = u32;
	type Data = u32;
}

#[derive(Component, Default)]
struct Producer {
	requests: Vec<u32>,
	drops: Vec<u32>,
}

impl DataProducingNode<Num> for Producer {
	fn cost(&self, _request: &Request<Num>) -> Option<f32> { Some(1.0) }
	fn request(&mut self, request: Request<Num>) { self.requests.push(request.params); }
	fn drop(&mut self, params: &u32) { self.drops.push(*params); }
}

#[derive(Component, Default)]
struct Consumer {
	received: Vec<(u32, u32)>,
	dirtied: Vec<u32>,
}

impl DataConsumingNode<Num> for Consumer {
	fn pass_data(&mut self, params: &u32, _tick: u64, data: u32) { self.received.push((*params, data)); }
	fn dirty_data(&mut self, params: &u32, _last_clean_tick: u64) { self.dirtied.push(*params); }
}

struct Setup {
	app: App,
	producer: Entity,
	consumers: [Entity; 2],
}

impl Setup {
	fn new() -> Self {
		let mut app = App::new();
		app.add_plugins(BileDataPlugin)
			.bile_add_data_type::<Num>()
			.register_component_as::<dyn DataProducingNode<Num>, Producer>()
			.register_component_as::<dyn DataConsumingNode<Num>, Consumer>();
		let producer = app.world_mut().spawn(Producer::default()).id();
		let consumers = [app.world_mut().spawn(Consumer::default()).id(), app.world_mut().spawn(Consumer::default()).id()];
		Self { app, producer, consumers }
	}

	fn request(&mut self, consumer: Entity, params: u32) -> Option<u32> {
		self.app.world_mut()
			.run_system_once(move |mut requester: DataRequester<Num>| requester.request(Request::new(params, DirtyState::UpToDate), consumer))
			.unwrap()
	}

	fn cancel(&mut self, consumer: Entity, params: u32) {
		self.app.world_mut().run_system_once(move |mut requester: DataRequester<Num>| requester.cancel(&params, consumer)).unwrap();
	}

	fn pass(&mut self, params: u32, data: u32) {
		let producer = self.producer;
		self.app.world_mut()
			.run_system_once(move |mut passer: DataPasser<Num>| passer.pass_data(producer, &params, DirtyState::UpToDate, data))
			.unwrap();
	}

	fn run_data_schedule(&mut self) {
		self.app.world_mut().run_schedule(BileDataSchedule);
	}

	fn producer(&self) -> &Producer { self.app.world().get::<Producer>(self.producer).unwrap() }
	fn consumer(&self, index: usize) -> &Consumer { self.app.world().get::<Consumer>(self.consumers[index]).unwrap() }
}

#[test]
fn in_flight_request_is_not_repeated() {
	let mut setup = Setup::new();
	let [a, b] = setup.consumers;
	assert_eq!(setup.request(a, 7), None);
	assert_eq!(setup.request(b, 7), None);
	assert_eq!(setup.producer().requests, vec![7]);

	setup.pass(7, 70);
	setup.run_data_schedule();
	assert_eq!(setup.consumer(0).received, vec![(7, 70)]);
	assert_eq!(setup.consumer(1).received, vec![(7, 70)]);
}

#[test]
fn cancelled_in_flight_request_reaches_producer() {
	let mut setup = Setup::new();
	let [a, _] = setup.consumers;
	setup.request(a, 3);
	setup.cancel(a, 3);
	setup.run_data_schedule();
	assert_eq!(setup.producer().drops, vec![3]);

	setup.request(a, 3);
	assert_eq!(setup.producer().requests, vec![3, 3]);
}

#[test]
fn late_delivery_after_cancel_is_dropped() {
	let mut setup = Setup::new();
	let [a, _] = setup.consumers;
	setup.request(a, 4);
	setup.cancel(a, 4);
	setup.pass(4, 40);
	setup.run_data_schedule();
	assert_eq!(setup.producer().drops, vec![4]);
	assert!(setup.consumer(0).received.is_empty());
}

#[test]
fn rerequest_before_drop_keeps_in_flight_request() {
	let mut setup = Setup::new();
	let [a, b] = setup.consumers;
	setup.request(a, 5);
	setup.cancel(a, 5);
	setup.request(b, 5);
	setup.run_data_schedule();
	assert_eq!(setup.producer().requests, vec![5]);
	assert!(setup.producer().drops.is_empty());

	setup.pass(5, 50);
	setup.run_data_schedule();
	assert!(setup.consumer(0).received.is_empty());
	assert_eq!(setup.consumer(1).received, vec![(5, 50)]);
}

#[test]
fn unused_data_is_dropped_once_every_consumer_cancels() {
	let mut setup = Setup::new();
	let [a, b] = setup.consumers;
	setup.request(a, 1);
	setup.pass(1, 10);
	setup.run_data_schedule();
	assert_eq!(setup.request(b, 1), Some(10));

	setup.cancel(a, 1);
	setup.run_data_schedule();
	assert!(setup.producer().drops.is_empty());

	setup.cancel(b, 1);
	setup.run_data_schedule();
	assert_eq!(setup.producer().drops, vec![1]);
	assert!(setup.app.world().resource::<DataColumn<Num>>().get(&1).is_none());
}

#[test]
fn mark_dirty_on_first_tick_does_not_underflow() {
	let mut setup = Setup::new();
	let [a, _] = setup.consumers;
	setup.request(a, 2);
	setup.pass(2, 20);
	setup.run_data_schedule();
	setup.app.world_mut().resource_mut::<StateTick>().current_tick = 0;
	setup.app.world_mut().run_system_once(|mut dirtier: DataDirtier<Num>| dirtier.mark_dirty(&2)).unwrap();
	assert_eq!(setup.consumer(0).dirtied, vec![2]);
	assert_eq!(setup.app.world().resource::<DataColumn<Num>>().get(&2).unwrap().dirty_state, DirtyState::Dirty { tick: 0 });
}
