pub mod default;
pub mod diff;
pub mod gather;
pub mod simple;
pub mod streaming;

use bevy::prelude::*;

use crate::{default::BileDataDefaultPlugin};

pub use default::{StateTick, DataType, StreamingDataType, DirtyState, Request, BileDataSchedule, BileDataSystemSet};

pub trait BileAddDataType {
	fn bile_add_data_type<DT: DataType>(&mut self) -> &mut Self;
}

#[derive(Default)]
pub struct BileDataPlugin;

impl Plugin for BileDataPlugin {
	fn build(&self, app: &mut App) {
		app.add_plugins(BileDataDefaultPlugin);
	}
}
#[cfg(test)]
mod tests;
