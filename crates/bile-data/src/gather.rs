// TODO
// Make a new producer trait that is for gathering from all producer nodes.
// This means that each node does not need to provide all the data that is needed, just its own resposibility.
// Look at github.com/ItchyTrack/Voxel-Game-Engine voxel-streaming for an example of where this might be used.
// (Only the gather step. This does not manage ownership transfer)
// Then create a new storage that impls the request API for streaming nodes. This allows other node types to
// provide the data if they can do it better than all the gather nodes together.
