mod region_loader;
mod coverage;
mod lod_bands;
mod systems;
mod tile_lifecycle;

use bevy::prelude::*;
use bile_tiles::{BileTilesSchedule, BileTilesSystemSet};

pub use crate::region_loader::RegionLoader;

#[derive(Default)]
pub struct RegionLoaderPlugin;

impl Plugin for RegionLoaderPlugin {
	fn build(&self, app: &mut App) {
		app.add_systems(
				BileTilesSchedule,
				(
					(
						systems::update_region_loader_requests,
						systems::refresh_region_loader_visibility
					).chain().in_set(BileTilesSystemSet::UpdateWantedClasses),
					systems::receive_region_loader_results.after(BileTilesSystemSet::UpdateWantedComponents)
				)
			);
	}
}
