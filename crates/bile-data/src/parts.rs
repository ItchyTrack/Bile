use std::{
	any::{TypeId, type_name},
	collections::{HashMap, HashSet},
	marker::PhantomData,
};

use bevy::prelude::*;

use crate::setup::DataType;

// Every data type is moved around by three parts:
// - The requester brings the data to whoever asks for it.
// - The storage keeps the data and everything the other parts need to remember about it.
// - The fulfiller gets the data from the producing nodes.
// The requester and the fulfiller are generic over the storage, each of them defines the trait a storage has to impl
// to be used with it. So a storage can change where the data is kept without the requester and fulfiller knowing.
// There is one set of parts per data type, all of them in one `DataParts` component.

/// A requester, storage or fulfiller.
pub trait Part: Default + Send + Sync + 'static {}

impl<T: Default + Send + Sync + 'static> Part for T {}

#[derive(Component)]
pub struct DataParts<DT: DataType, R: Part, S: Part, F: Part> {
	pub requester: R,
	pub storage: S,
	pub fulfiller: F,
	_marker: PhantomData<fn() -> DT>,
}

impl<DT: DataType, R: Part, S: Part, F: Part> Default for DataParts<DT, R, S, F> {
	fn default() -> Self { Self { requester: R::default(), storage: S::default(), fulfiller: F::default(), _marker: PhantomData } }
}

/// Implemented by every requester: adds what its kind of parts needs to the app (the entry points, their params and
/// their systems). It is called once for every set of parts.
pub trait SetupParts<DT: DataType, S, F> {
	fn setup(app: &mut App);
}

pub trait BileSetDataParts {
	/// Moves `DT` around with these parts, replacing the ones it had. Only for while building the app, the replaced
	/// parts lose what they stored.
	fn set_data_parts<DT, R, S, F>(&mut self) -> &mut Self
	where DT: DataType, R: Part + SetupParts<DT, S, F>, S: Part, F: Part;
	/// Sets the parts only if `DT` has none yet. Registering a node uses this for the default parts of its kind.
	fn init_data_parts<DT, R, S, F>(&mut self) -> &mut Self
	where DT: DataType, R: Part + SetupParts<DT, S, F>, S: Part, F: Part;
}

impl BileSetDataParts for App {
	fn set_data_parts<DT, R, S, F>(&mut self) -> &mut Self
	where DT: DataType, R: Part + SetupParts<DT, S, F>, S: Part, F: Part,
	{
		let parts_type = TypeId::of::<DataParts<DT, R, S, F>>();
		let mut registry = self.world_mut().get_resource_or_init::<DataPartsRegistry>();
		let is_new = registry.set_up.insert(parts_type);
		let existing = registry.entities.get(&TypeId::of::<DT>()).copied();
		if existing.is_some_and(|existing| existing.parts_type == parts_type) { return self; }

		let entity = match existing {
			Some(existing) => {
				let mut entity = self.world_mut().entity_mut(existing.entity);
				(existing.remove)(&mut entity);
				entity.insert(DataParts::<DT, R, S, F>::default()).id()
			}
			None => self.world_mut().spawn((Name::new(format!("{} data parts", type_name::<DT>())), DataParts::<DT, R, S, F>::default())).id(),
		};
		let remove = |entity: &mut EntityWorldMut| { entity.remove::<DataParts<DT, R, S, F>>(); };
		self.world_mut().resource_mut::<DataPartsRegistry>().entities.insert(TypeId::of::<DT>(), PartsEntity { entity, parts_type, remove });
		if is_new {
			R::setup(self);
		}
		self
	}

	fn init_data_parts<DT, R, S, F>(&mut self) -> &mut Self
	where DT: DataType, R: Part + SetupParts<DT, S, F>, S: Part, F: Part,
	{
		let has_parts = self.world().get_resource::<DataPartsRegistry>().is_some_and(|registry| registry.entities.contains_key(&TypeId::of::<DT>()));
		if !has_parts {
			self.set_data_parts::<DT, R, S, F>();
		}
		self
	}
}

#[derive(Clone, Copy)]
struct PartsEntity {
	entity: Entity,
	parts_type: TypeId,
	remove: fn(&mut EntityWorldMut),
}

#[derive(Resource, Default)]
struct DataPartsRegistry {
	entities: HashMap<TypeId, PartsEntity>, // the entity with the parts of every data type
	set_up: HashSet<TypeId>,                // the sets of parts that were added to the app
}
