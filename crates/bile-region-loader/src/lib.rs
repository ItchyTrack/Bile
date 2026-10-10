mod region_loader;
mod coverage;
mod lod_bands;
mod systems;
mod tile_lifecycle;
mod types;
mod unresolved_tile_index;

use bevy::prelude::*;

pub use crate::region_loader::{RegionLoader, CoverageDebugState, CoverageDebugTile};

#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct FreezeRegionLoader(pub bool);

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum RegionLoaderSet {
	RefreshVisibility,
}

#[derive(Default)]
pub struct RegionRegionLoaderPlugin;

impl Plugin for RegionRegionLoaderPlugin {
	fn build(&self, app: &mut App) {
		app.init_resource::<FreezeRegionLoader>()
			.add_systems(
				Update,
				systems::update_region_loader_requests
					.in_set(StreamingPhase::Request)
					.before(RegionLoaderSet::RefreshVisibility),
			)
			.add_systems(StreamingSchedule, systems::receive_region_loader_results.after(StreamingPhase::Receive))
			.add_systems(
				Update,
				systems::refresh_region_loader_visibility.in_set(RegionLoaderSet::RefreshVisibility),
			);
	}
}
