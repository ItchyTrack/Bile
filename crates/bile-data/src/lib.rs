pub mod default;
pub mod single_cached;

use bevy::prelude::*;

use crate::{default::BileDataDefaultPlugin, single_cached::BileAddDataTypeSingleCached};

pub use default::{StateTick, DataType, DirtyState, Request, BileDataSchedule, BileDataSystemSet};

pub trait BileAddDataType {
	fn bile_add_data_type<DT: DataType>(&mut self) -> &mut Self;
}

impl BileAddDataType for App {
	fn bile_add_data_type<DT: DataType>(&mut self) -> &mut Self {
		self.bile_add_data_type_single_cached::<DT>()
	}
}

#[derive(Default)]
pub struct BileDataPlugin;

impl Plugin for BileDataPlugin {
	fn build(&self, app: &mut App) {
		app.add_plugins(BileDataDefaultPlugin);
	}
}
