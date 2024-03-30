use {
    bussy::{Connection, Object, PendingReply},
    isnt::std_1::primitive::IsntStrExt,
    parking_lot::Mutex,
    std::{collections::HashSet, sync::Arc},
    zbus::{
        names::{InterfaceName, MemberName, WellKnownName},
        zvariant::ObjectPath,
    },
};

pub const FDO_WATCHER_NAME: WellKnownName<'static> =
    WellKnownName::from_static_str_unchecked("org.freedesktop.StatusNotifierWatcher");
pub static FDO_WATCHER_INTERFACE: InterfaceName<'static> =
    InterfaceName::from_static_str_unchecked("org.freedesktop.StatusNotifierWatcher");
pub const KDE_WATCHER_NAME: WellKnownName<'static> =
    WellKnownName::from_static_str_unchecked("org.kde.StatusNotifierWatcher");
pub static KDE_WATCHER_INTERFACE: InterfaceName<'static> =
    InterfaceName::from_static_str_unchecked("org.kde.StatusNotifierWatcher");
pub const WATCHER_PATH: ObjectPath<'static> =
    ObjectPath::from_static_str_unchecked("/StatusNotifierWatcher");
pub const REGISTER_STATUS_NOTIFIER_ITEM: MemberName<'static> =
    MemberName::from_static_str_unchecked("RegisterStatusNotifierItem");
pub const REGISTER_STATUS_NOTIFIER_HOST: MemberName<'static> =
    MemberName::from_static_str_unchecked("RegisterStatusNotifierHost");
pub const STATUS_NOTIFIER_ITEM_REGISTERED: MemberName<'static> =
    MemberName::from_static_str_unchecked("StatusNotifierItemRegistered");
pub const STATUS_NOTIFIER_ITEM_UNREGISTERED: MemberName<'static> =
    MemberName::from_static_str_unchecked("StatusNotifierItemUnregistered");
pub const STATUS_NOTIFIER_HOST_REGISTERED: MemberName<'static> =
    MemberName::from_static_str_unchecked("StatusNotifierHostRegistered");
pub const IS_STATUS_NOTIFIER_HOST_REGISTERED: MemberName<'static> =
    MemberName::from_static_str_unchecked("IsStatusNotifierHostRegistered");
pub const PROTOCOL_VERSION: MemberName<'static> =
    MemberName::from_static_str_unchecked("ProtocolVersion");
pub const REGISTERED_STATUS_NOTIFIER_ITEMS: MemberName<'static> =
    MemberName::from_static_str_unchecked("RegisteredStatusNotifierItems");

#[derive(Default)]
struct DataMut {
    items: HashSet<String>,
    hosts: HashSet<String>,
}

struct Data {
    fdo: Mutex<DataMut>,
    kde: Mutex<DataMut>,
    dbus: Arc<Connection>,
    obj: Arc<Object>,
}

impl Data {
    fn data(&self, fdo: bool) -> &Mutex<DataMut> {
        match fdo {
            true => &self.fdo,
            false => &self.kde,
        }
    }

    fn register_status_notifier_item(&self, fdo: bool, service_or_path: &str, pr: &PendingReply) {
        let item_string;
        let item = if fdo {
            service_or_path
        } else {
            if service_or_path.starts_with("/") {
                if let Some(sender) = pr.sender() {
                    item_string = format!("{}{}", sender.as_str(), service_or_path);
                    &item_string
                } else {
                    return;
                }
            } else {
                item_string = format!("{}{}", service_or_path, "/StatusNotifierItem");
                &item_string
            }
        };
        let new_item = {
            let mut data = self.data(fdo).lock();
            data.items.insert(item.to_string())
        };
        if new_item {
            let items: Vec<_> = self.data(fdo).lock().items.iter().cloned().collect();
            let int = match fdo {
                true => &FDO_WATCHER_INTERFACE,
                false => &KDE_WATCHER_INTERFACE,
            };
            self.obj
                .set_property(int, &REGISTERED_STATUS_NOTIFIER_ITEMS, items.clone());
            self.dbus
                .send_signal(int, &WATCHER_PATH, &STATUS_NOTIFIER_ITEM_REGISTERED, &item);
        }
    }

    pub fn register_status_notifier_host(&self, fdo: bool, service: &str) {
        let new_host;
        let first_host;
        {
            let mut data = self.data(fdo).lock();
            new_host = data.hosts.insert(service.to_string());
            first_host = new_host && data.hosts.len() == 1;
        }
        if new_host {
            let int = match fdo {
                true => &FDO_WATCHER_INTERFACE,
                false => &KDE_WATCHER_INTERFACE,
            };
            self.dbus
                .send_signal(int, &WATCHER_PATH, &STATUS_NOTIFIER_HOST_REGISTERED, &());
            if first_host {
                self.obj
                    .set_property(int, &IS_STATUS_NOTIFIER_HOST_REGISTERED, true);
            }
        }
    }

    fn handle_name_owner_changed(&self, name: String, _old_owner: String, new_owner: String) {
        if new_owner.is_not_empty() {
            return;
        }
        if name == FDO_WATCHER_INTERFACE.as_str() {
            self.dbus.request_name(FDO_WATCHER_NAME);
        }
        if name == KDE_WATCHER_INTERFACE.as_str() {
            self.dbus.request_name(KDE_WATCHER_NAME);
        }
        {
            let mut fdo = self.fdo.lock();
            if fdo.items.remove(&name) {
                self.dbus.send_signal(
                    &FDO_WATCHER_INTERFACE,
                    &WATCHER_PATH,
                    &STATUS_NOTIFIER_ITEM_UNREGISTERED,
                    &name,
                );
                self.obj.set_property(
                    &FDO_WATCHER_INTERFACE,
                    &REGISTERED_STATUS_NOTIFIER_ITEMS,
                    fdo.items.iter().cloned().collect::<Vec<_>>(),
                )
            }
        }
        {
            let mut kde = self.kde.lock();
            let mut removed_any = false;
            kde.items.retain(|item| {
                let remove = item.starts_with(&name);
                if remove {
                    self.dbus.send_signal(
                        &KDE_WATCHER_INTERFACE,
                        &WATCHER_PATH,
                        &STATUS_NOTIFIER_ITEM_UNREGISTERED,
                        item,
                    );
                }
                removed_any |= remove;
                !remove
            });
            if removed_any {
                self.obj.set_property(
                    &KDE_WATCHER_INTERFACE,
                    &REGISTERED_STATUS_NOTIFIER_ITEMS,
                    kde.items.iter().cloned().collect::<Vec<_>>(),
                )
            }
        }
        for fdo in [true, false] {
            let int = match fdo {
                true => &FDO_WATCHER_INTERFACE,
                false => &KDE_WATCHER_INTERFACE,
            };
            let hosts_emptied = {
                let mut data = self.data(fdo).lock();
                let removed = data.hosts.remove(&name);
                removed && data.hosts.is_empty()
            };
            if hosts_emptied {
                self.obj
                    .set_property(int, &IS_STATUS_NOTIFIER_HOST_REGISTERED, false);
            }
        }
    }
}

pub fn create_watcher(dbus: &Arc<Connection>) {
    let obj = dbus.add_obj(&WATCHER_PATH);
    let watcher = Arc::new(Data {
        fdo: Default::default(),
        kde: Default::default(),
        dbus: dbus.clone(),
        obj,
    });
    let w = watcher.clone();
    dbus.on_name_owner_changed(move |name, old_owner, new_owner| {
        w.handle_name_owner_changed(name, old_owner, new_owner);
    })
    .detach();
    dbus.request_name(FDO_WATCHER_NAME);
    dbus.request_name(KDE_WATCHER_NAME);
    for fdo in [true, false] {
        let interface = match fdo {
            true => &FDO_WATCHER_INTERFACE,
            false => &KDE_WATCHER_INTERFACE,
        };
        let w = watcher.clone();
        watcher.obj.add_method(
            interface,
            REGISTER_STATUS_NOTIFIER_ITEM,
            move |a: String, mut pr| {
                w.register_status_notifier_item(fdo, &a, &pr);
                pr.send(&());
            },
        );
        let w = watcher.clone();
        watcher.obj.add_method(
            interface,
            &REGISTER_STATUS_NOTIFIER_HOST,
            move |a: String, mut pr| {
                w.register_status_notifier_host(fdo, &a);
                pr.send(&());
            },
        );
        watcher
            .obj
            .set_property(interface, &IS_STATUS_NOTIFIER_HOST_REGISTERED, false);
        watcher.obj.set_property(interface, &PROTOCOL_VERSION, 0i32);
        watcher.obj.set_property(
            interface,
            &REGISTERED_STATUS_NOTIFIER_ITEMS,
            Vec::<String>::new(),
        );
    }
}
