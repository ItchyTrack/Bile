mod node;
mod bounds;

pub use node::SceneNode;
pub use bounds::NodeBounds;

// The idea for bile scenes is simple. Add a bile datastreaming node that impls DataType
// with the DataType being (data being a scene node bounds and params being the scene node id)
// and the params being the parent node id. This node adds the children as they come in as child
// entities to the parent. This allows a request for the nodes within a scene node. Then also we
// want to add a bile-tiles like class system but for the scene nodes which would allow loading
// the content of nodes. A way this could be used is to have a root node and then load parts of
// the world recursively. We would want to impl something like region loader but it loads based
// on global regions where it loads part of the scene to depths based on the LOD. Tile could be
// loaded on scene nodes that are not leafs.

// All this should NOT be done now and will need a lot more thinking. Just writing this down to keep some documentation.
