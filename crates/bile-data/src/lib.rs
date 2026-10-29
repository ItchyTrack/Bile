use std::collections::VecDeque;

use bevy::{ecs::system::SystemParam, prelude::*};
use downcast_rs::{Downcast, impl_downcast};

pub trait RequestKey: Downcast { }
impl_downcast!(RequestKey);

pub struct Request {
	tick: u64, // increase everytime anything happens in bile
	key: Box<dyn RequestKey>
}

impl Request {
	pub fn tick(&self) -> u64 { self.tick }
	pub fn key(&self) -> &dyn RequestKey { self.key.as_ref() }
	pub fn key_as<T: RequestKey>(&self) -> Option<&T> { self.key.downcast_ref() }
	// pub fn take_key(self) -> Box<dyn RequestKey> { self.key }
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
	queue: &'requester mut VecDeque<Request>,
}

impl<'requester> DataRequesterQueue<'requester> {
	pub fn request(&mut self, request: Request) {
		self.queue.push_back(request);
	}
}

#[bevy_trait_query::queryable]
pub trait DataProcessingNode {
	fn cost(&self, request: &Request) -> Option<f32>; // None if the node can not fulfill the request
	fn request<'requester>(&mut self, request: Request, data_requester_queue: &mut DataRequesterQueue<'requester>);
	fn drop(&mut self, request: &Request); // this both cancels requests and stops dirting from propagating up.
	fn pass_data(&mut self, request: &Request, data: Box<dyn Data>);
	fn dirty_data(&mut self, request: &Request); // will be passed the diff later
}

#[derive(SystemParam)]
pub struct DataRequester<'w, 's> {
	data_processing_node: Query<'w, 's, &'static mut dyn DataProcessingNode>,
}

impl<'w, 's> DataRequester<'w, 's> {
	pub fn request(&mut self, request: Request) {
		let mut queue = VecDeque::default();
		queue.push_back(request);
		while let Some(request) = queue.pop_back() {
			let mut best = (f32::MAX, None);
			for node in self.data_processing_node.iter_mut().flatten() {
				let Some(cost) = node.cost(&request) else { continue; };
				if best.0 > cost {
					best.0 = cost;
					best.1 = Some(node);
				}
			}
			let Some(mut node) = best.1 else {
				panic!("Data request failed to find node to fulfill the request. This should never happen!");
			};
			node.request(request, &mut DataRequesterQueue{ queue: &mut queue });
		}
	}
}
