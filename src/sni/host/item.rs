use {
    crate::sni::{
        host::{
            menu::{Menu, DBUS_MENU},
            Host,
        },
        SniMenuDelta,
    },
    arc_swap::ArcSwapOption,
    bussy::{Call, CallFuture, MatchRuleBuilder, SignalHandler},
    parking_lot::Mutex,
    std::{
        error::Error,
        fmt::{Debug, Formatter},
        sync::Arc,
        time::UNIX_EPOCH,
    },
    wayland_client::protocol::wl_pointer::Axis,
    zbus::{
        names::{BusName, InterfaceName, MemberName},
        zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Type, Value},
    },
};

#[derive(Value, OwnedValue, Type)]
pub struct IconPixmap {
    pub width: i32,
    pub height: i32,
    pub bytes: Vec<u8>,
}

impl From<Vec<IconPixmap>> for IconFrames {
    fn from(value: Vec<IconPixmap>) -> Self {
        IconFrames {
            frames: Arc::new(
                value
                    .into_iter()
                    .filter(|p| {
                        p.width > 0
                            && p.height > 0
                            && p.bytes.len() as u64 >= p.width as u64 * p.height as u64 * 4
                    })
                    .map(|p| IconFrame {
                        bytes: p.bytes,
                        size: (p.width, p.height),
                    })
                    .collect(),
            ),
        }
    }
}

impl Debug for IconPixmap {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IconPixmap")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

#[derive(Value, OwnedValue, Type, Debug)]
pub struct Tooltip {
    pub icon_name: String,
    pub icon_data: Vec<IconPixmap>,
    pub title: String,
    pub text: String,
}

static ITEM_KDE: InterfaceName<'static> =
    InterfaceName::from_static_str_unchecked("org.kde.StatusNotifierItem");
static ITEM_FDO: InterfaceName<'static> =
    InterfaceName::from_static_str_unchecked("org.freedesktop.StatusNotifierItem");

const PROP_CATEGORY: MemberName<'static> = MemberName::from_static_str_unchecked("Category");
const PROP_ID: MemberName<'static> = MemberName::from_static_str_unchecked("Id");
const PROP_TITLE: MemberName<'static> = MemberName::from_static_str_unchecked("Title");
const PROP_STATUS: MemberName<'static> = MemberName::from_static_str_unchecked("Status");
const PROP_ICON_NAME: MemberName<'static> = MemberName::from_static_str_unchecked("IconName");
const PROP_ICON_THEME_PATH: MemberName<'static> =
    MemberName::from_static_str_unchecked("IconThemePath");
const PROP_ICON_PIXMAP: MemberName<'static> = MemberName::from_static_str_unchecked("IconPixmap");
const PROP_OVERLAY_ICON_NAME: MemberName<'static> =
    MemberName::from_static_str_unchecked("OverlayIconName");
const PROP_OVERLAY_ICON_PIXMAP: MemberName<'static> =
    MemberName::from_static_str_unchecked("OverlayIconPixmap");
const PROP_ATTENTION_ICON_NAME: MemberName<'static> =
    MemberName::from_static_str_unchecked("AttentionIconName");
const PROP_ATTENTION_ICON_PIXMAP: MemberName<'static> =
    MemberName::from_static_str_unchecked("AttentionIconPixmap");
const PROP_ATTENTION_MOVIE_NAME: MemberName<'static> =
    MemberName::from_static_str_unchecked("AttentionMovieName");
const PROP_TOOL_TIP: MemberName<'static> = MemberName::from_static_str_unchecked("ToolTip");
const PROP_MENU: MemberName<'static> = MemberName::from_static_str_unchecked("Menu");
const PROP_ITEM_IS_MENU: MemberName<'static> = MemberName::from_static_str_unchecked("ItemIsMenu");

const ACTIVATE: MemberName<'static> = MemberName::from_static_str_unchecked("Activate");
const SECONDARY_ACTIVATE: MemberName<'static> =
    MemberName::from_static_str_unchecked("SecondaryActivate");
const SCROLL: MemberName<'static> = MemberName::from_static_str_unchecked("Scroll");
const EVENT: MemberName<'static> = MemberName::from_static_str_unchecked("Event");
const ABOUT_TO_SHOW: MemberName<'static> = MemberName::from_static_str_unchecked("AboutToShow");

const SIG_NEW_TITLE: MemberName<'static> = MemberName::from_static_str_unchecked("NewTitle");
const SIG_NEW_ICON: MemberName<'static> = MemberName::from_static_str_unchecked("NewIcon");
const SIG_NEW_ATTENTION_ICON: MemberName<'static> =
    MemberName::from_static_str_unchecked("NewAttentionIcon");
const SIG_NEW_OVERLAY_ICON: MemberName<'static> =
    MemberName::from_static_str_unchecked("NewOverlayIcon");
const SIG_NEW_TOOL_TIP: MemberName<'static> = MemberName::from_static_str_unchecked("NewToolTip");
const SIG_NEW_STATUS: MemberName<'static> = MemberName::from_static_str_unchecked("NewStatus");

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MutableProperty {
    Title,
    Icon,
    AttentionIcon,
    OverlayIcon,
    ToolTip,
    Status,
}

pub trait SniItemOwner: Send + Sync {
    fn removed(&self);

    fn property_changed(&self, prop: MutableProperty);

    fn menu_changed(&self, delta: SniMenuDelta);
}

#[derive(Eq, PartialEq)]
pub struct IconFrame {
    pub bytes: Vec<u8>,
    pub size: (i32, i32),
}

impl Debug for IconFrame {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SniFrame")
            .field("width", &self.size.0)
            .field("height", &self.size.1)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Default, Clone, Eq, PartialEq)]
pub struct IconFrames {
    pub frames: Arc<Vec<IconFrame>>,
}

#[derive(Debug, Default, Clone)]
pub struct SniItemProperties {
    pub category: Option<Arc<String>>,
    pub id: Option<Arc<String>>,
    pub title: Option<Arc<String>>,
    pub status: Option<Arc<String>>,
    pub icon_name: Option<Arc<String>>,
    pub icon_theme_path: Option<Arc<String>>,
    pub icon: Option<IconFrames>,
    pub attention_icon_name: Option<Arc<String>>,
    pub attention_icon: Option<IconFrames>,
    pub overlay_icon_name: Option<Arc<String>>,
    pub overlay_icon: Option<IconFrames>,
    pub attention_movie_name: Option<Arc<String>>,
    pub tooltip: Option<Arc<Tooltip>>,
    pub is_menu: bool,
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum ItemStatus {
    New,
    Announced,
    Removed,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct SniItemId(u64);

pub struct SniItem {
    id: SniItemId,
    destination: BusName<'static>,
    interface: &'static InterfaceName<'static>,
    path: ObjectPath<'static>,
    host: Arc<Host>,
    pub(in crate::sni) owner: ArcSwapOption<Box<dyn SniItemOwner>>,
    properties: Mutex<SniItemProperties>,
    status: Mutex<ItemStatus>,
    signal_handlers: Mutex<Vec<SignalHandler>>,
    pub(in crate::sni) menu: Mutex<Option<Menu>>,
}

impl SniItem {
    pub fn id(&self) -> SniItemId {
        self.id
    }

    pub fn properties(&self) -> SniItemProperties {
        self.properties.lock().clone()
    }

    fn activate_(&self, member: MemberName, cb: impl FnOnce(bool) + Send + 'static) -> Call {
        self.host.dbus.call_async(
            &self.destination,
            self.interface,
            &self.path,
            member,
            &(0i32, 0i32),
            move |res: Result<(), _>| {
                cb(res.is_ok());
            },
        )
    }

    pub fn activate(&self, cb: impl FnOnce(bool) + Send + 'static) -> Call {
        self.activate_(ACTIVATE, cb)
    }

    pub fn secondary_activate(&self, cb: impl FnOnce(bool) + Send + 'static) -> Call {
        self.activate_(SECONDARY_ACTIVATE, cb)
    }

    pub fn scroll(&self, delta: i32, axis: Axis) {
        let orientation = match axis {
            Axis::VerticalScroll => "vertical",
            Axis::HorizontalScroll => "horizontal",
            _ => return,
        };
        self.host.dbus.call_no_reply(
            &self.destination,
            self.interface,
            &self.path,
            SCROLL,
            &(delta, orientation),
        )
    }

    fn menu_event(&self, event: &str, menu_id: i32) {
        let menu = self.menu.lock();
        let Some(menu) = &*menu else {
            return;
        };
        let now = UNIX_EPOCH.elapsed().unwrap_or_default().as_secs() as u32;
        self.host.dbus.call_no_reply(
            &self.destination,
            DBUS_MENU,
            &menu.path,
            EVENT,
            &(menu_id, event, Value::U8(0), now),
        )
    }

    pub fn menu_hovered(&self, menu_id: i32) {
        self.menu_event("hovered", menu_id);
    }

    pub fn menu_clicked(&self, menu_id: i32) {
        self.menu_event("clicked", menu_id);
    }

    pub fn open_menu(
        self: &Arc<Self>,
        menu_id: i32,
        callback: Box<dyn FnOnce() + Send + Sync>,
    ) -> Option<Call> {
        let menu = self.menu.lock();
        let Some(menu) = &*menu else {
            return None;
        };
        let item = self.clone();
        let call = self.host.dbus.call_async(
            &self.destination,
            DBUS_MENU,
            &menu.path,
            ABOUT_TO_SHOW,
            &menu_id,
            move |res: Result<bool, _>| {
                let Ok(res) = res else {
                    callback();
                    return;
                };
                if res {
                    if let Some(menu) = &mut *item.menu.lock() {
                        menu.update_layout(&item, menu_id, Some(callback));
                    }
                } else {
                    callback();
                }
            },
        );
        Some(call)
    }

    pub fn set_owner(&self, owner: Box<dyn SniItemOwner>) {
        self.owner.store(Some(Arc::new(owner)));
    }

    fn get_prop<T>(&self, name: MemberName<'_>) -> CallFuture<T>
    where
        T: TryFrom<OwnedValue> + Send + 'static,
        T::Error: Error + Send + Sync + 'static,
    {
        self.host
            .dbus
            .get_property(&self.destination, self.interface, &self.path, name)
    }
}

impl Debug for SniItem {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let props = self.properties.lock();
        f.debug_struct("SniItem")
            .field("props", &*props)
            .finish_non_exhaustive()
    }
}

impl Host {
    pub fn handle_new_item(self: &Arc<Self>, fdo: bool, id: &str) {
        if self.items.lock().contains_key(id) {
            return;
        }
        let (destination, path) = match id.find("/") {
            None => (id, "/StatusNotifierItem"),
            Some(p) => (&id[..p], &id[p..]),
        };
        let Ok(destination) = BusName::try_from(destination.to_string()) else {
            return;
        };
        let Ok(path) = ObjectPath::try_from(path.to_string()) else {
            return;
        };
        let interface = match fdo {
            true => &ITEM_FDO,
            false => &ITEM_KDE,
        };
        let item = Arc::new(SniItem {
            id: {
                static IDS: Mutex<u64> = Mutex::new(0);
                let mut ids = IDS.lock();
                *ids += 1;
                SniItemId(*ids)
            },
            destination,
            interface,
            path,
            host: self.clone(),
            owner: Default::default(),
            properties: Default::default(),
            signal_handlers: Default::default(),
            status: Mutex::new(ItemStatus::New),
            menu: Default::default(),
        });
        let mut signal_handlers = vec![];
        macro_rules! handle_signal {
            ($sig:ident, $sty:ty, $mutable:ident, [$($prop:ident, $field:ident, $ty:ty;)+]) => {{
                let i1 = item.clone();
                let handler = self.dbus.handle_messages(
                    MatchRuleBuilder::default()
                        .interface(interface)
                        .member($sig)
                        .sender(&item.destination)
                        .path(&item.path)
                        .build(),
                    move |_: $sty| {
                        let i2 = i1.clone();
                        tokio::spawn(async move {
                            $(
                                let $field =
                                    i2.host
                                        .dbus
                                        .get_property::<$ty>(
                                            &i2.destination,
                                            i2.interface,
                                            &i2.path,
                                            $prop,
                                        );
                            )+
                            $(
                                let $field = $field.await.ok().map(|v| v.into());
                            )+
                            {
                                let props = &mut *i2.properties.lock();
                                $(
                                    props.$field = $field;
                                )+
                            }
                            if let Some(owner) = &*i2.owner.load() {
                                owner.property_changed(MutableProperty::$mutable);
                            }
                        });
                    },
                );
                signal_handlers.push(handler);
            }};
        }
        handle_signal!(SIG_NEW_TITLE, (), Title, [
            PROP_TITLE, title, String;
        ]);
        handle_signal!(SIG_NEW_ICON, (), Icon, [
            PROP_ICON_PIXMAP, icon, Vec<IconPixmap>;
            PROP_ICON_NAME, icon_name, String;
        ]);
        handle_signal!(SIG_NEW_ATTENTION_ICON, (), AttentionIcon, [
            PROP_ATTENTION_ICON_PIXMAP, attention_icon, Vec<IconPixmap>;
            PROP_ATTENTION_ICON_NAME, attention_icon_name, String;
        ]);
        handle_signal!(SIG_NEW_OVERLAY_ICON, (), OverlayIcon, [
            PROP_OVERLAY_ICON_PIXMAP, overlay_icon, Vec<IconPixmap>;
            PROP_OVERLAY_ICON_NAME, overlay_icon_name, String;
        ]);
        handle_signal!(SIG_NEW_TOOL_TIP, (), ToolTip, [
            PROP_TOOL_TIP, tooltip, Tooltip;
        ]);
        handle_signal!(SIG_NEW_STATUS, String, Status, [
            PROP_STATUS, status, String;
        ]);
        *item.signal_handlers.lock() = signal_handlers;
        self.items.lock().insert(id.to_string(), item.clone());
        tokio::spawn(async move {
            macro_rules! get {
                ($($name:ident, $member:ident, $ty:ty;)*) => {
                    $(
                        let $name = item.get_prop::<$ty>($member);
                    )*
                    $(
                        let $name = $name.await.ok();
                    )*
                    {
                        let mut props = item.properties.lock();
                        $(
                            props.$name = $name.map(|v| v.into());
                        )*
                    }
                };
            }
            let menu = item.get_prop::<OwnedObjectPath>(PROP_MENU);
            let is_menu = item.get_prop::<bool>(PROP_ITEM_IS_MENU);
            get! {
                category, PROP_CATEGORY, String;
                id, PROP_ID, String;
                title, PROP_TITLE, String;
                status, PROP_STATUS, String;
                icon_name, PROP_ICON_NAME, String;
                icon_theme_path, PROP_ICON_THEME_PATH, String;
                icon, PROP_ICON_PIXMAP, Vec<IconPixmap>;
                attention_icon_name, PROP_ATTENTION_ICON_NAME, String;
                attention_movie_name, PROP_ATTENTION_MOVIE_NAME, String;
                attention_icon, PROP_ATTENTION_ICON_PIXMAP, Vec<IconPixmap>;
                overlay_icon_name, PROP_OVERLAY_ICON_NAME, String;
                overlay_icon, PROP_OVERLAY_ICON_PIXMAP, Vec<IconPixmap>;
                tooltip, PROP_TOOL_TIP, Tooltip;
            }
            item.properties.lock().is_menu = matches!(is_menu.await, Ok(true));
            let menu_path: Option<OwnedObjectPath> = menu.await.ok();
            let mut menu_delta = None;
            if let Some(path) = menu_path {
                let menu = Menu::new(&item, &item.host.dbus, &item.destination, &path).await;
                if let Some(menu) = &menu {
                    menu_delta = Some(menu.tree.clone().into());
                }
                *item.menu.lock() = menu;
            }
            {
                let mut status = item.status.lock();
                if *status == ItemStatus::New {
                    (item.host.callback)(&item);
                    if let Some(delta) = menu_delta {
                        if let Some(owner) = &*item.owner.load() {
                            owner.menu_changed(delta);
                        }
                    }
                }
                *status = ItemStatus::Announced;
            }
        });
    }

    pub fn handle_removed_item(self: &Arc<Self>, _fdo: bool, id: &str) {
        let Some(item) = self.items.lock().remove(id) else {
            return;
        };
        *item.status.lock() = ItemStatus::Removed;
        if let Some(owner) = item.owner.swap(None) {
            owner.removed();
        }
        item.signal_handlers.lock().clear();
        item.menu.lock().take();
    }
}
