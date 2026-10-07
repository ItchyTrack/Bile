use bevy::prelude::*;
use bevy::ecs::{component::ComponentId};
use bile_tiles::{ComponentSource, TileKey};

#[derive(Debug, Resource)]
pub struct RenderMeshComponentSource {
	mesh3d_component_id: ComponentId,
	// mesh3d: HashMap<Mesh3d, Entity, TileKey>, // we will use Cuboid::new(1.0, 1.0, 1.0) for now
	work: Vec<(Entity, TileKey)>,
}

impl RenderMeshComponentSource {
	pub fn new(mesh3d_component_id: ComponentId) -> Self {
		Self {
			mesh3d_component_id,
			work: default(),
		}
	}
}

impl ComponentSource for RenderMeshComponentSource {
	fn component_cost(&self, _entity: Entity, _tile_key: TileKey, component: ComponentId) -> Option<f32>  {
		if component == self.mesh3d_component_id { Some(1.0) } else { None }
	}

	fn request_component(&mut self, _entity: Entity, tile_entity: Entity, tile_key: TileKey, component: ComponentId) {
		assert!(component == self.mesh3d_component_id);
		self.work.push((tile_entity, tile_key));
	}
}

fn do_work(
	mut render_mesh_component_source: ResMut<RenderMeshComponentSource>,
	mut meshes: ResMut<Assets<Mesh>>,
	mut commands: Commands,
) {
	for (tile_entity, _tile_key) in render_mesh_component_source.work.drain(..) {
		let mesh_handle = meshes.add(Cuboid::new(1.0, 1.0, 1.0));
		commands.entity(tile_entity).insert(Mesh3d(mesh_handle));
	}
}

#[derive(Default)]
pub struct RenderMeshComponentSourcePlugin;

impl Plugin for RenderMeshComponentSourcePlugin {
	fn build(&self, app: &mut App) {
		let mesh3d_component_id = app.world_mut().register_component::<Mesh3d>();
		app
			.insert_resource(RenderMeshComponentSource::new(mesh3d_component_id))
			.add_systems(Update, do_work);
	}
}
