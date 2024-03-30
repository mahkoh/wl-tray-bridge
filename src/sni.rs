pub use host::{
    item::{
        IconFrame, IconFrames, MutableProperty, SniItem, SniItemId, SniItemOwner, SniItemProperties,
    },
    menu::{SniMenuDelta, SniMenuToggleType},
};
use {bussy::Connection, std::sync::Arc};

mod host;
mod watcher;

pub fn spawn<CB>(conn: &Arc<Connection>, cb: CB)
where
    CB: Fn(&Arc<SniItem>) + Send + Sync + 'static,
{
    watcher::create_watcher(conn);
    host::create_hosts(conn, cb);
}
