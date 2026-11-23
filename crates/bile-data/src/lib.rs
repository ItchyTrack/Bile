#![allow(type_alias_bounds)]

pub mod setup;
pub mod diff;
pub mod dyn_system_params;
pub mod simple_api;
pub mod simple_consuming;
pub mod simple_producing;
pub mod simple_storage;
pub mod streaming_api;
pub mod btree_drain;
pub mod cancel_range;
// pub mod streaming_consuming;
// pub mod streaming_producing;
// pub mod streaming;
// pub mod gather;
// #[cfg(test)]
// mod streaming_tests;

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
