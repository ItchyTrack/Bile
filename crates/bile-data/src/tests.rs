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
}

impl DataConsumingNode<Num> for Consumer {
	fn pass_data(&mut self, params: &u32, _tick: u64, data: u32) { self.received.push((*params, data)); }
	fn dirty_data(&mut self, _params: &u32, _last_clean_tick: u64) {}
}

struct Setup {
	app: App,
	producer: Entity,
	consumers: [Entity; 2],
}

impl Setup {
	fn new() -> Self {
		let mut app = App::new();
		app.add_plugins(BileDataPlugin::default());
		app.bile_add_data_type::<Num>();
		app.register_component_as::<dyn DataProducingNode<Num>, Producer>()
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
fn pending_request_is_not_repeated() {
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
fn delivery_after_cancel_is_dropped() {
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
fn rerequest_after_cancel_is_not_dropped() {
	let mut setup = Setup::new();
	let [a, b] = setup.consumers;
	setup.request(a, 1);
	setup.pass(1, 10);
	setup.run_data_schedule();

	setup.cancel(a, 1);
	assert_eq!(setup.request(b, 1), None);
	setup.run_data_schedule();
	assert_eq!(setup.producer().requests, vec![1, 1]);
	assert!(setup.producer().drops.is_empty());
}
