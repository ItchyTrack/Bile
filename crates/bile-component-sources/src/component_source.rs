use bevy::{ecs::component::ComponentId, prelude::*};
use bile_tiles::TileKey;

use crate::component_request::ComponentRequestId;

#[bevy_trait_query::queryable]
pub trait ComponentSource {
	fn component_cost(&mut self, entity: Entity, tile_key: TileKey, component: ComponentId) -> Option<f32>;
	fn request_component(&mut self, entity: Entity, tile_key: TileKey, component: ComponentId, request_id: ComponentRequestId);
	fn cancel_request(&mut self, request_id: ComponentRequestId);
}
