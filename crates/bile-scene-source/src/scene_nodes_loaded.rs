use bevy::prelude::*;

#[derive(Debug, Message)]
pub struct SceneNodesLoaded {
	entity: Entity,
}
