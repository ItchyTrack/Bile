use bevy::prelude::*;

#[derive(Debug, Message)]
pub struct SceneNodesLoaded {
	_entity: Entity,
}
