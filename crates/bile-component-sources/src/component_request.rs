use bevy::prelude::*;
use bile_tiles::TileKey;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ComponentRequestId(pub(crate) i64);

#[derive(Debug, Clone, Copy, Message)]
pub struct FinishedRequestMessage {
	pub entity: Entity,
	pub tile_key: TileKey,
	pub request_id: ComponentRequestId,
}
