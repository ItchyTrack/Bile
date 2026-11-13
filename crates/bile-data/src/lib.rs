pub mod default;
pub mod single_cached;
pub mod single_uncached;

use bevy::prelude::*;

use crate::{default::BileDataDefaultPlugin};

pub use default::{StateTick, DataType, DirtyState, Request, BileDataSchedule, BileDataSystemSet};

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
