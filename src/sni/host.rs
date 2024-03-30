use {
    crate::sni::{
        host::item::SniItem,
        watcher::{
            FDO_WATCHER_INTERFACE, FDO_WATCHER_NAME, KDE_WATCHER_INTERFACE, KDE_WATCHER_NAME,
            REGISTERED_STATUS_NOTIFIER_ITEMS, REGISTER_STATUS_NOTIFIER_HOST,
            STATUS_NOTIFIER_ITEM_REGISTERED, STATUS_NOTIFIER_ITEM_UNREGISTERED, WATCHER_PATH,
        },
    },
    ahash::AHashMap,
    bussy::Connection,
    isnt::std_1::primitive::IsntStrExt,
    parking_lot::Mutex,
    rand::random,
    std::sync::Arc,
    zbus::names::WellKnownName,
};

pub mod item;
pub mod menu;

type NewItemHandler = Box<dyn Fn(&Arc<SniItem>) + Send + Sync>;

struct Host {
    dbus: Arc<Connection>,
    fdo_name: WellKnownName<'static>,
    kde_name: WellKnownName<'static>,
    callback: NewItemHandler,
    items: Mutex<AHashMap<String, Arc<SniItem>>>,
}

impl Host {
    fn handle_name_owner_changed(self: &Arc<Self>, name: &str) {
        for fdo in [true, false] {
            let (watcher_name, interface, host_name) = match fdo {
                true => (FDO_WATCHER_NAME, &FDO_WATCHER_INTERFACE, &self.fdo_name),
                false => (KDE_WATCHER_NAME, &KDE_WATCHER_INTERFACE, &self.kde_name),
            };
            if name == watcher_name.as_str() {
                self.dbus.call_no_reply(
                    watcher_name.clone(),
                    interface,
                    WATCHER_PATH,
                    REGISTER_STATUS_NOTIFIER_HOST,
                    &host_name.as_str(),
                );
                let h = self.clone();
                self.dbus
                    .get_property_async(
                        watcher_name,
                        interface,
                        WATCHER_PATH,
                        REGISTERED_STATUS_NOTIFIER_ITEMS,
                        move |res: Result<Vec<String>, _>| {
                            if let Ok(ids) = res {
                                for id in ids {
                                    h.handle_new_item(fdo, &id);
                                }
                            }
                        },
                    )
                    .detach();
            }
        }
    }
}

pub fn create_hosts<CB>(dbus: &Arc<Connection>, cb: CB)
where
    CB: Fn(&Arc<SniItem>) + Send + Sync + 'static,
{
    let id: u64 = random();
    let name = |proto: &str| {
        WellKnownName::from_str_unchecked(&format!("org.{proto}.StatusNotifierHost-{id:016x}"))
            .to_owned()
    };
    let host = Arc::new(Host {
        dbus: dbus.clone(),
        fdo_name: name("freedesktop"),
        kde_name: name("kde"),
        callback: Box::new(cb),
        items: Default::default(),
    });
    for fdo in [true, false] {
        let int = match fdo {
            true => &FDO_WATCHER_INTERFACE,
            false => &KDE_WATCHER_INTERFACE,
        };
        let h = host.clone();
        dbus.handle_signal(int, STATUS_NOTIFIER_ITEM_REGISTERED, move |v: String| {
            h.handle_new_item(fdo, &v);
        })
        .detach();
        let h = host.clone();
        dbus.handle_signal(int, STATUS_NOTIFIER_ITEM_UNREGISTERED, move |v: String| {
            h.handle_removed_item(fdo, &v);
        })
        .detach();
    }
    dbus.request_name(&host.fdo_name);
    dbus.request_name(&host.kde_name);
    let h = host.clone();
    dbus.on_name_owner_changed(move |name, _old, new| {
        if new.is_not_empty() {
            h.handle_name_owner_changed(&name);
        }
    })
    .detach();
    host.handle_name_owner_changed(&FDO_WATCHER_NAME);
    host.handle_name_owner_changed(&KDE_WATCHER_NAME);
}
