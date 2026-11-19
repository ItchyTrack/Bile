use std::{
	any::{Any, TypeId, type_name},
	collections::{HashMap, HashSet},
};

use bevy::{ecs::component::Mutable, prelude::*};
use bevy_trait_query::RegisterExt;

use crate::setup::DataType;

// Every data type is moved around by one storage and any number of requesters and fulfillers. Each of them is its own
// component on the entity of the data type:
// - The storage keeps the data and which requests for it are active.
// - A requester brings the data to whoever asks for it, like the consuming nodes or the entities that want it.
// - A fulfiller gets the data from its kind of producing nodes. When more than one could, the cheapest one does.
// Requesters and fulfillers are generic over the storage, each of them defines the trait a storage has to impl to be
// used with it. What only one of them needs to remember it keeps itself.

/// A storage, requester or fulfiller.
pub trait Part: Component<Mutability = Mutable> + Default {}

impl<T: Component<Mutability = Mutable> + Default> Part for T {}

/// Keeps the data of `DT` and which requests for it are active.
pub trait Storage<DT: DataType>: Part {
	/// Adds what the storage needs to the app, once.
	fn setup(_app: &mut App) {}
}

/// A requester or fulfiller of `DT`.
pub trait DataPart<DT: DataType>: Part {
	type Storage: Storage<DT>;

	/// Adds what the part needs to the app (its entry points, params and systems), once.
	fn setup(app: &mut App);
}

/// How the parts reach the storage of their data type without knowing its type.
#[bevy_trait_query::queryable]
pub trait DataStorage<DT: DataType> {
	fn as_any(&self) -> &dyn Any;
	fn as_any_mut(&mut self) -> &mut dyn Any;
}

impl<DT: DataType, S: Storage<DT>> DataStorage<DT> for S {
	fn as_any(&self) -> &dyn Any { self }
	fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

impl<'a, DT: DataType> dyn DataStorage<DT> + 'a {
	pub fn downcast_ref<S: 'static>(&self) -> &S { self.as_any().downcast_ref().unwrap_or_else(|| wrong_storage::<DT, S>()) }
	pub fn downcast_mut<S: 'static>(&mut self) -> &mut S { self.as_any_mut().downcast_mut().unwrap_or_else(|| wrong_storage::<DT, S>()) }
}

fn wrong_storage<DT, S>() -> ! {
	panic!("{} does not keep its data in {}. A part is only on the entity of the storage it was added for.", type_name::<DT>(), type_name::<S>())
}

pub trait BileDataParts {
	/// Keeps `DT` in `S`. A different storage replaces the one `DT` had together with all its requesters and
	/// fulfillers, they only work with the storage they were added for. Only for while building the app.
	fn set_data_storage<DT: DataType, S: Storage<DT>>(&mut self) -> &mut Self;
	/// Adds `P` to `DT`, setting its storage if `DT` has none yet. Panics if `DT` keeps its data in another storage.
	fn add_data_part<DT: DataType, P: DataPart<DT>>(&mut self) -> &mut Self;
	/// Adds `P` only if `DT` keeps its data in the storage of `P`, setting it if `DT` has none yet. Registering a
	/// node uses this for the default parts of its kind.
	fn init_data_part<DT: DataType, P: DataPart<DT>>(&mut self) -> &mut Self;
}

impl BileDataParts for App {
	fn set_data_storage<DT: DataType, S: Storage<DT>>(&mut self) -> &mut Self {
		let world = self.world_mut();
		let mut data_types = world.get_resource_or_init::<DataTypes>();
		let first_setup = data_types.set_up.insert(TypeId::of::<(DT, S)>());
		match data_types.entries.get_mut(&TypeId::of::<DT>()) {
			Some(entry) if entry.storage == TypeId::of::<S>() => {}
			Some(entry) => {
				let entity = entry.entity;
				let removes = std::mem::take(&mut entry.removes);
				*entry = DataTypeEntry::new::<S>(entity);
				let mut entity = world.entity_mut(entity);
				for remove in removes {
					remove(&mut entity);
				}
				entity.insert(S::default());
			}
			None => {
				let entity = world.spawn((Name::new(format!("{} data", type_name::<DT>())), S::default())).id();
				world.resource_mut::<DataTypes>().entries.insert(TypeId::of::<DT>(), DataTypeEntry::new::<S>(entity));
			}
		}
		if first_setup {
			self.register_component_as::<dyn DataStorage<DT>, S>();
			S::setup(self);
		}
		self
	}

	fn add_data_part<DT: DataType, P: DataPart<DT>>(&mut self) -> &mut Self {
		match storage_of::<DT>(self) {
			None => { self.set_data_storage::<DT, P::Storage>(); }
			Some(storage) if storage == TypeId::of::<P::Storage>() => {}
			Some(_) => panic!("{} keeps its data in {}, but {} does not keep it there.", type_name::<P>(), type_name::<P::Storage>(), type_name::<DT>()),
		}
		insert_part::<DT, P>(self)
	}

	fn init_data_part<DT: DataType, P: DataPart<DT>>(&mut self) -> &mut Self {
		match storage_of::<DT>(self) {
			None => { self.set_data_storage::<DT, P::Storage>(); }
			Some(storage) if storage == TypeId::of::<P::Storage>() => {}
			Some(_) => return self,
		}
		insert_part::<DT, P>(self)
	}
}

fn storage_of<DT: DataType>(app: &App) -> Option<TypeId> {
	app.world().get_resource::<DataTypes>()?.entries.get(&TypeId::of::<DT>()).map(|entry| entry.storage)
}

fn insert_part<DT: DataType, P: DataPart<DT>>(app: &mut App) -> &mut App {
	let world = app.world_mut();
	let mut data_types = world.resource_mut::<DataTypes>();
	let first_setup = data_types.set_up.insert(TypeId::of::<(DT, P)>());
	let entry = data_types.entries.get_mut(&TypeId::of::<DT>()).expect("The storage is set before its parts");
	if entry.parts.insert(TypeId::of::<P>()) {
		let entity = entry.entity;
		entry.removes.push(|entity| { entity.remove::<P>(); });
		world.entity_mut(entity).insert(P::default());
	}
	if first_setup {
		P::setup(app);
	}
	app
}

#[derive(Resource, Default)]
struct DataTypes {
	entries: HashMap<TypeId, DataTypeEntry>, // of every data type
	set_up: HashSet<TypeId>,                 // the storages and parts of each data type that were added to the app
}

struct DataTypeEntry {
	entity: Entity, // with the storage and parts
	storage: TypeId,
	parts: HashSet<TypeId>,
	removes: Vec<fn(&mut EntityWorldMut)>, // the storage and every part
}

impl DataTypeEntry {
	fn new<S: Component>(entity: Entity) -> Self {
		Self { entity, storage: TypeId::of::<S>(), parts: HashSet::new(), removes: vec![|entity| { entity.remove::<S>(); }] }
	}
}
