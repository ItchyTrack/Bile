mod coverage;
mod lod_bands;
mod region_loader;
mod systems;
mod tile_lifecycle;
#[cfg(test)]
mod tests;

use std::marker::PhantomData;

use bevy::{prelude::*, transform::TransformSystems};
use bevy_trait_query::RegisterExt;
use bile_data::{DataConsumingNode, DataRequester, DataType, DirtyState, Request};
use bile_math::NonZeroRegion;
use bile_tiles::TileKey;

pub use crate::lod_bands::LodBand;
pub use crate::region_loader::RegionLoader;

#[derive(Debug, Clone, Copy, Message)]
pub struct RegionUpdated {
	pub entity: Entity,
	pub region: NonZeroRegion,
}

pub trait TileContent: Clone + Send + Sync + 'static {
	fn spawn(self, commands: &mut Commands, loader: Entity) -> Option<Entity>;
}

impl<B: Bundle + Clone> TileContent for Option<B> {
	fn spawn(self, commands: &mut Commands, loader: Entity) -> Option<Entity> {
		Some(commands.spawn((self?, ChildOf(loader))).id())
	}
}

#[derive(Component, Deref, DerefMut)]
pub struct RegionLoaderComponent<DT: DataType> {
	#[deref]
	loader: RegionLoader,
	received: Vec<(TileKey, DT::Data)>,
}

impl<DT: DataType> RegionLoaderComponent<DT> {
	pub fn new(tile_size: u32) -> Self {
		Self { loader: RegionLoader::new(tile_size), received: Vec::new() }
	}
}

impl<DT: DataType<Params = TileKey>> RegionLoaderComponent<DT> where DT::Data: TileContent {
	fn flush(&mut self, entity: Entity, commands: &mut Commands, requester: &mut DataRequester<DT>) {
		for key in self.loader.take_acquired() {
			if let Some(data) = requester.request(Request::new(key, DirtyState::UpToDate), entity) {
				self.received.push((key, data));
			}
		}
		for (key, data) in std::mem::take(&mut self.received) {
			if !self.loader.is_requested(key) { continue; }
			let tile_entity = data.spawn(commands, entity);
			self.loader.tile_loaded(key, tile_entity);
		}
		for (key, resolution) in self.loader.take_released() {
			requester.cancel(&key, entity);
			if let Some(tile_entity) = resolution.entity() {
				commands.entity(tile_entity).try_despawn();
			}
		}
	}
}

impl<DT: DataType<Params = TileKey>> DataConsumingNode<DT> for RegionLoaderComponent<DT> {
	fn pass_data(&mut self, params: &TileKey, _tick: u64, data: DT::Data) {
		self.received.push((*params, data));
	}
	fn dirty_data(&mut self, _params: &TileKey, _last_clean_tick: u64) {}
}

pub struct RegionLoaderPlugin<DT>(PhantomData<fn() -> DT>);

impl<DT> Default for RegionLoaderPlugin<DT> {
	fn default() -> Self {
		Self(PhantomData)
	}
}

impl<DT: DataType<Params = TileKey>> Plugin for RegionLoaderPlugin<DT> where DT::Data: TileContent {
	fn build(&self, app: &mut App) {
		app.add_message::<RegionUpdated>()
			.init_resource::<systems::RemovedLoaderTiles<DT>>()
			.register_component_as::<dyn DataConsumingNode<DT>, RegionLoaderComponent<DT>>()
			.add_systems(PostUpdate, systems::update_loaders::<DT>.before(TransformSystems::Propagate))
			.add_observer(systems::release_removed_loader::<DT>);
	}
}
