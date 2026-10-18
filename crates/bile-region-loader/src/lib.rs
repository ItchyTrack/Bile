mod coverage;
mod key;
mod lod_bands;
mod region_loader;
mod systems;
mod tile_lifecycle;

use std::marker::PhantomData;

use bevy::prelude::*;
use bile_math::NonZeroRegion;
use bile_tiles::{BileTilesSchedule, BileTilesSystemSet, TileInstances};

pub use crate::key::TileClassKey;
pub use crate::lod_bands::LodBand;
pub use crate::region_loader::RegionLoader;

#[derive(Debug, Clone, Copy, Message)]
pub struct RegionUpdated {
	pub entity: Entity,
	pub region: NonZeroRegion,
}

#[derive(Component, Deref, DerefMut)]
#[require(TileInstances)]
pub struct RegionLoaderComponent<M: 'static> {
	#[deref]
	loader: RegionLoader,
	marker: PhantomData<fn() -> M>,
}

impl<M: 'static> RegionLoaderComponent<M> {
	pub fn new(tile_size: u32) -> Self {
		Self::from_loader(RegionLoader::new(tile_size))
	}
	pub fn from_loader(loader: RegionLoader) -> Self {
		Self { loader, marker: PhantomData }
	}
	pub fn into_inner(self) -> RegionLoader {
		self.loader
	}
}

impl<M: 'static> std::fmt::Debug for RegionLoaderComponent<M> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_tuple(std::any::type_name::<Self>()).field(&self.loader).finish()
	}
}

pub struct RegionLoaderPlugin<M: 'static>(PhantomData<fn() -> M>);

impl<M: 'static> Default for RegionLoaderPlugin<M> {
	fn default() -> Self {
		Self(PhantomData)
	}
}

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RegionLoaderSystemSet {
	Receive,
	Update,
}

impl<M: 'static> Plugin for RegionLoaderPlugin<M> {
	fn build(&self, app: &mut App) {
		app.add_message::<RegionUpdated>()
			.configure_sets(
				BileTilesSchedule,
				(RegionLoaderSystemSet::Receive, RegionLoaderSystemSet::Update)
					.chain()
					.in_set(BileTilesSystemSet::UpdateWantedClasses),
			)
			.add_systems(
				BileTilesSchedule,
				(
					(systems::receive_ready_classes::<M>, systems::receive_region_updates::<M>).in_set(RegionLoaderSystemSet::Receive),
					systems::update_requests::<M>.in_set(RegionLoaderSystemSet::Update),
				),
			)
			.add_observer(systems::release_removed_loader::<M>);
	}
}
