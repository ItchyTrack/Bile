pub mod setup;
pub mod diff;
pub mod dyn_system_params;
pub mod gather;
pub mod simple_consuming;
pub mod simple_producing;
pub mod simple_storage;
pub mod simple_impl;
pub mod streaming_consuming;
pub mod streaming_producing;
pub mod streaming;

use bevy::prelude::*;

use crate::{setup::BileDataDefaultPlugin};

pub use setup::{StateTick, Request, DirtyState, DataType, BileDataSchedule, BileDataSystemSet};

#[derive(Default)]
pub struct BileDataPlugin;

impl Plugin for BileDataPlugin {
	fn build(&self, app: &mut App) {
		app.add_plugins(BileDataDefaultPlugin);
	}
}

#[cfg(test)]
mod tests;
