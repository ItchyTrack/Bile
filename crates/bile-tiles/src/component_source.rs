use bevy::{ecs::component::ComponentId, prelude::*};

use crate::TileKey;

#[bevy_trait_query::queryable]
pub trait ComponentSource {
	fn component_cost(&self, entity: Entity, tile_key: TileKey, component: ComponentId) -> Option<f32>;
	fn request_component(&mut self, entity: Entity, tile_entity: Entity, tile_key: TileKey, component: ComponentId);
	// fn cancel_request(&mut self, entity: Entity, tile_entity: Entity, tile_key: TileKey, component: ComponentId); // Will add once needed
}

#[derive(Debug, Clone, Copy, Message)]
pub struct FinishedRequestMessage {
	pub entity: Entity,
	pub tile_entity: Entity,
	pub tile_key: TileKey,
	pub component_id: ComponentId,
}
