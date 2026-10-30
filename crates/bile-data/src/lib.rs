use std::{any::TypeId, collections::{HashMap, VecDeque}};

use bevy::{ecs::system::SystemParam, prelude::*};
use bevy_trait_query::One;
use downcast_rs::{Downcast, impl_downcast};

pub trait RequestKey: Downcast { }
impl_downcast!(RequestKey);

pub struct Request {
	tick: u64, // increase everytime anything happens in bile
	type_id: TypeId,
	key: Box<dyn RequestKey>
}

impl Request {
	pub fn tick(&self) -> u64 { self.tick }
	pub fn type_id(&self) -> TypeId { self.type_id }
	pub fn key(&self) -> &dyn RequestKey { self.key.as_ref() }
	pub fn key_as<T: RequestKey>(&self) -> Option<&T> { self.key.downcast_ref() }
}

pub trait Data: Downcast {
	fn tick(&self) -> u64;
}
impl_downcast!(Data);

pub struct DataOf<T> {
	tick: u64,
	data: T,
}

impl<T: 'static> DataOf<T> {
	pub fn get(&self) -> &T { &self.data }
	pub fn take(self) -> T { self.data }
}

impl<T: 'static> Data for DataOf<T> {
	fn tick(&self) -> u64 { self.tick }
}

pub struct DataRequesterQueue<'requester> {
	next_request_id: &'requester mut RequestId,
	queue: &'requester mut VecDeque<(Request, RequestId)>,
}

impl<'requester> DataRequesterQueue<'requester> {
	pub fn request(&mut self, request: Request) -> RequestId {
		let this_request_id = *self.next_request_id;
		self.queue.push_back((request, this_request_id));
		self.next_request_id.0 += 1;
		this_request_id
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(pub(crate) u64);

// There should only be one DataProcessingNode per entity
#[bevy_trait_query::queryable]
pub trait DataProcessingNode {
	fn cost(&self, request: &Request) -> Option<f32>; // None if the node can not fulfill the request
	fn request<'requester>(&mut self, request: Request, request_id: RequestId, data_requester_queue: &mut DataRequesterQueue<'requester>);
	fn drop(&mut self, request: RequestId); // this both cancels requests and stops dirting from propagating up.
	fn pass_data(&mut self, request_id: RequestId, data: Box<dyn Data>);
	fn dirty_data(&mut self, request: &Request); // will be passed the diff later
	fn drain_finished(&mut self) -> Vec<(RequestId, Box<dyn Data>)>;
}

#[derive(Resource)]
pub struct RequestTracker {
	next_request_id: RequestId,
	waiting: HashMap<RequestId, Entity>,
}

#[derive(SystemParam)]
pub struct DataRequester<'w, 's> {
	request_tracker: ResMut<'w, RequestTracker>,
	data_processing_nodes: Query<'w, 's, (One<&'static mut dyn DataProcessingNode>, Entity)>,
}

impl<'w, 's> DataRequester<'w, 's> {
	pub fn request(&mut self, request: Request, requester_node: Option<Entity>) -> RequestId {
		let this_request_id = self.request_tracker.next_request_id;
		self.request_tracker.next_request_id.0 += 1;
		self.request_force_id(request, this_request_id, requester_node);
		this_request_id
	}
	fn request_force_id(&mut self, request: Request, this_request_id: RequestId, requester_node: Option<Entity>) {
		// get node
		let node_entity = Self::best_node(&self.data_processing_nodes, &request);
		let mut node = self.data_processing_nodes.get_mut(node_entity).expect("This must exist as it was found in best_node").0;
		// update requester
		if let Some(requester_node) = requester_node { self.request_tracker.waiting.insert(this_request_id, requester_node); }
		// create queue
		let mut queue = VecDeque::default();
		// request dat
		node.request(request, this_request_id, &mut DataRequesterQueue{
			next_request_id: &mut self.request_tracker.next_request_id,
			queue: &mut queue,
		});
		// loop over all queued requests
		for (request, request_id) in queue.drain(..) {
			// request data
			self.request_force_id(request, request_id, Some(node_entity));
		}
	}
	fn best_node(data_processing_nodes: &Query<'w, 's, (One<&'static mut dyn DataProcessingNode>, Entity)>, request: &Request) -> Entity {
		let mut best = (f32::MAX, None);
		for (node, entity) in data_processing_nodes.iter() {
			let Some(cost) = node.cost(request) else { continue; };
			if best.0 > cost {
				best.0 = cost;
				best.1 = Some(entity);
			}
		}
		let Some(entity) = best.1 else {
			panic!("Data request failed to find node to fulfill the request. This should never happen!");
		};
		entity
	}
}

pub fn pass_data(
	request_tracker: ResMut<RequestTracker>,
	mut data_processing_nodes: Query<One<&'static mut dyn DataProcessingNode>>,
) {
	for mut node in data_processing_nodes.iter_mut() {
		for (request_id, data) in node.drain_finished().drain(..) {
			let Some(&target_entity) = request_tracker.waiting.get(&request_id) else { continue; };
			let Ok(mut target_node) = data_processing_nodes.get_mut(target_entity) else { continue; };
			target_node.pass_data(request_id, data);
		}
	}
}
