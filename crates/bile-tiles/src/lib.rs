mod key;
mod index;
mod components;
mod component_source;
mod tile_management;

use bevy::prelude::*;
pub use components::DesiredComponents;
pub use index::{TileIndex, TileIndexKey, TileKeyMap};
pub use key::TileKey;
pub use component_source::{ComponentSource, FinishedRequestMessage};
pub use tile_management::TileInstances;

#[derive(Default)]
pub struct TileDataPlugin;

impl Plugin for TileDataPlugin {
	fn build(&self, app: &mut App) {
		app.add_systems(Update,
			(
				tile_management::modify_tile_classes,
				(
					components::remove_newly_undesired_components,
					components::request_newly_desired
				),
			).chain()
		);
	}
}

pub trait RegisterTileClass {
    fn register_tile_class<T: Component>(&mut self) -> &mut Self;
}

impl RegisterTileClass for App {
    fn register_tile_class<T: Component>(&mut self) -> &mut Self {
        self.add_systems(Update, components::remove_desired_components_for_removed_classes::<T>)
    }
}
