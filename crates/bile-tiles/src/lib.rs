mod key;
mod index;
mod components;
mod component_source;
mod tile_management;

use bevy::{app::MainScheduleOrder, ecs::schedule::ScheduleLabel, prelude::*};
pub use components::{DesiredComponents, TileClassReady};
pub use index::{TileIndex, TileIndexKey, TileKeyMap, TileKeySet};
pub use key::TileKey;
pub use component_source::{ComponentSource, FinishedRequestMessage};
pub use tile_management::TileInstances;

#[derive(ScheduleLabel, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BileTilesSchedule;

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BileTilesSystemSet {
	/// Finished component requests are applied and [`TileClassReady`] messages are sent.
	UpdateReadyClasses,
	/// Requesters add and remove classes on [`TileInstances`].
	UpdateWantedClasses,
	ClassRequest,
	/// Systems that decide which components a class needs (usually by querying `Added<Class>`)
	/// should call [`DesiredComponents::add_desired_component`] here.
	UpdateWantedComponents,
	ComponentRequest,
}

#[derive(Default)]
pub struct TileDataPlugin;

impl Plugin for TileDataPlugin {
	fn build(&self, app: &mut App) {
		app.init_schedule(BileTilesSchedule);
		app.world_mut()
			.resource_mut::<MainScheduleOrder>()
			.insert_after(Update, BileTilesSchedule);

		app.configure_sets(
			BileTilesSchedule,
			(
				BileTilesSystemSet::UpdateReadyClasses,
				BileTilesSystemSet::UpdateWantedClasses,
				BileTilesSystemSet::ClassRequest,
				BileTilesSystemSet::UpdateWantedComponents,
				BileTilesSystemSet::ComponentRequest,
			).chain(),
		);

		app.add_message::<FinishedRequestMessage>()
			.add_message::<TileClassReady>();

		app.add_systems(
			BileTilesSchedule,
			(
				components::update_ready_classes.in_set(BileTilesSystemSet::UpdateReadyClasses),
				tile_management::modify_tile_classes.in_set(BileTilesSystemSet::ClassRequest),
				(
					components::remove_newly_undesired_components,
					components::request_newly_desired,
				).in_set(BileTilesSystemSet::ComponentRequest),
			),
		);
	}
}

pub trait RegisterTileClass {
	fn register_tile_class<T: Component>(&mut self) -> &mut Self;
}

impl RegisterTileClass for App {
	fn register_tile_class<T: Component>(&mut self) -> &mut Self {
		self.add_systems(
			BileTilesSchedule,
			components::remove_desired_components_for_removed_classes::<T>.in_set(BileTilesSystemSet::ComponentRequest),
		)
	}
}