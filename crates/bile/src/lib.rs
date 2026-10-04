use bevy::app::plugin_group;

plugin_group! {
    // / This plugin group will add all the default plugins for a *Bile* application:
    pub struct DefaultBilePlugins {
		bile_tiles:::TileDataPlugin,
	}
}
