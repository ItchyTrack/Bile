use crate::component_request::ComponentRequestId;

#[derive(Debug, Default)]
pub struct TileComponentSourceManager {
	last_request_id: ComponentRequestId,
}

impl TileComponentSourceManager {
	pub fn handle_finished_request(&mut self, request_id: ComponentRequestId) {

	}

	pub fn new_request_id(&mut self) -> ComponentRequestId {
		self.last_request_id.0 += 1;
		self.last_request_id
	}
}
