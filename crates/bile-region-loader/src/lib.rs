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

/// Tells loaders on `entity` that what exists inside `region` may have changed, see [`RegionLoader::region_updated`].
#[derive(Debug, Clone, Copy, Message)]
pub struct RegionUpdated {
	pub entity: Entity,
	pub region: NonZeroRegion,
}

/// A [`RegionLoader`] as a component on an entity with [`TileInstances`] and [`bile_scene::NodeBounds`].
/// `M` is a marker type so one entity can have several independent loaders. Add
/// [`RegionLoaderPlugin<M>`] to run it.
///
/// ```ignore
/// pub struct MainRendering;
/// pub type MainRenderingLoader = RegionLoaderComponent<MainRendering>;
/// app.add_plugins(RegionLoaderPlugin::<MainRendering>::default());
/// ```
#[derive(Component, Deref, DerefMut)]
#[require(TileInstances)]
pub struct RegionLoaderComponent<M: 'static> {
	#[deref]
	loader: RegionLoader,
	marker: PhantomData<fn() -> M>,
}

impl<M: 'static> RegionLoaderComponent<M> {
	/// `tile_size` is the size of a lod 0 tile in region units.
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

/// Runs every [`RegionLoaderComponent<M>`]. Requires [`bile_tiles::TileDataPlugin`].
pub struct RegionLoaderPlugin<M: 'static>(PhantomData<fn() -> M>);

impl<M: 'static> Default for RegionLoaderPlugin<M> {
	fn default() -> Self {
		Self(PhantomData)
	}
}

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RegionLoaderSystemSet {
	/// Loaders resolve tiles from [`bile_tiles::TileClassReady`] and apply [`RegionUpdated`] messages.
	Receive,
	/// Loaders apply bands set with [`RegionLoader::set_regions`]. Set regions before this.
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
