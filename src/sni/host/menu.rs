use {
    crate::sni::host::item::SniItem,
    ahash::{AHashMap, HashMap},
    bussy::{Call, Connection, MatchRuleBuilder, SignalHandler},
    indexmap::IndexMap,
    isnt::std_1::collections::IsntHashMapExt,
    serde::Deserialize,
    std::sync::Arc,
    zbus::{
        names::{BusName, InterfaceName, MemberName},
        zvariant::{Array, ObjectPath, OwnedValue, Type, Value},
    },
};

pub const DBUS_MENU: InterfaceName<'static> =
    InterfaceName::from_static_str_unchecked("com.canonical.dbusmenu");
pub const GET_LAYOUT: MemberName<'static> = MemberName::from_static_str_unchecked("GetLayout");
pub const LAYOUT_UPDATED: MemberName<'static> =
    MemberName::from_static_str_unchecked("LayoutUpdated");
pub const ITEMS_PROPERTIES_UPDATED: MemberName<'static> =
    MemberName::from_static_str_unchecked("ItemsPropertiesUpdated");

pub struct Menu {
    pub dbus: Arc<Connection>,
    pub destination: BusName<'static>,
    pub path: ObjectPath<'static>,
    pub revision: u32,
    pub tree: MenuTree,
    pub next_layout_update: u64,
    pub layout_updates: AHashMap<u64, Call>,
    pub _signals: Vec<SignalHandler>,
}

impl Drop for Menu {
    fn drop(&mut self) {
        self.layout_updates.clear();
    }
}

impl Menu {
    pub async fn new(
        item: &Arc<SniItem>,
        dbus: &Arc<Connection>,
        destination: &BusName<'static>,
        path: &ObjectPath<'static>,
    ) -> Option<Self> {
        let (revision, value) = dbus
            .call::<(u32, MenuLayoutValue)>(
                destination,
                DBUS_MENU,
                path,
                GET_LAYOUT,
                &(0i32, -1i32, Vec::<String>::new()),
            )
            .await
            .ok()?;
        let build = |member| {
            MatchRuleBuilder::default()
                .msg_type(zbus::message::Type::Signal)
                .sender(destination)
                .path(path)
                .interface(DBUS_MENU)
                .member(member)
                .build()
        };
        let i1 = item.clone();
        let s1 = dbus.handle_messages(
            build(LAYOUT_UPDATED),
            move |(revision, menu_id): (u32, i32)| {
                if let Some(menu) = &mut *i1.menu.lock() {
                    if menu.revision < revision {
                        menu.revision = revision;
                        menu.update_layout(&i1, menu_id, None);
                    }
                }
            },
        );
        let i2 = item.clone();
        let s2 = dbus.handle_messages(
            build(ITEMS_PROPERTIES_UPDATED),
            #[allow(clippy::type_complexity)]
            move |diff: (
                Vec<(i32, HashMap<String, OwnedValue>)>,
                Vec<(i32, Vec<String>)>,
            )| {
                if let Some(menu) = &mut *i2.menu.lock() {
                    if menu.layout_updates.is_not_empty() {
                        menu.update_layout(&i2, 0, None);
                    } else {
                        // if there is no pending call, then this signal was sent after
                        // the response to any previous layout query. therefore we can
                        // use the values without any race conditions.
                        menu.update_properties(&i2, diff.0, diff.1);
                    }
                }
            },
        );
        let tree = value.parse_tree();
        Some(Self {
            dbus: dbus.clone(),
            destination: destination.clone(),
            path: path.clone(),
            revision,
            tree,
            next_layout_update: 0,
            layout_updates: Default::default(),
            _signals: vec![s1, s2],
        })
    }

    pub fn update_layout(
        &mut self,
        item: &Arc<SniItem>,
        menu_id: i32,
        callback: Option<Box<dyn FnOnce() + Send + Sync>>,
    ) {
        let item = item.clone();
        self.next_layout_update += 1;
        let id = self.next_layout_update;
        let call = self.dbus.call_async::<(u32, MenuLayoutValue)>(
            &self.destination,
            DBUS_MENU,
            &self.path,
            GET_LAYOUT,
            &(menu_id, -1i32, Vec::<String>::new()),
            move |res: Result<(u32, MenuLayoutValue), _>| {
                let mut menu = item.menu.lock();
                if let Some(m) = &mut *menu {
                    m.layout_updates.remove(&id);
                    let Ok((_, v)) = res else {
                        return;
                    };
                    let tree = v.parse_tree();
                    let delta = m.tree.merge_nested(tree.menu_id, &mut Some(tree));
                    drop(menu);
                    if let Some(Some(delta)) = delta {
                        if let Some(owner) = &*item.owner.load() {
                            owner.menu_changed(delta);
                        }
                    }
                    if let Some(cb) = callback {
                        cb();
                    }
                }
            },
        );
        self.layout_updates.insert(id, call);
    }

    pub fn update_properties(
        &mut self,
        item: &Arc<SniItem>,
        changed: Vec<(i32, HashMap<String, OwnedValue>)>,
        removed: Vec<(i32, Vec<String>)>,
    ) {
        let mut diff = AHashMap::new();
        for (menu_id, old_map) in changed.into_iter() {
            let mut map = AHashMap::new();
            for (key, v) in old_map {
                map.insert(key, Some(v));
            }
            diff.insert(menu_id, map);
        }
        for (menu_id, props) in removed.into_iter() {
            let map = diff.entry(menu_id).or_default();
            for key in props {
                map.insert(key, None);
            }
        }
        if let Some(delta) = self.tree.merge_property_tree(&diff) {
            if let Some(owner) = &*item.owner.load() {
                owner.menu_changed(delta);
            }
        }
    }
}

#[derive(Default, Clone, Debug, Eq, PartialEq)]
struct MenuProperties {
    pub menu_id: i32,
    pub separator: bool,
    pub access_key: Option<char>,
    pub label: Arc<String>,
    pub enabled: bool,
    pub visible: bool,
    pub icon_name: Arc<String>,
    pub icon_png: Arc<Vec<u8>>,
    pub toggle_type: Option<SniMenuToggleType>,
    pub toggle_state: bool,
    pub children_display: bool,
}

trait PropertyGetter {
    fn get(&self, name: &str) -> Option<Option<&OwnedValue>>;
}

impl PropertyGetter for AHashMap<String, Option<OwnedValue>> {
    fn get(&self, name: &str) -> Option<Option<&OwnedValue>> {
        self.get(name).map(|v| v.as_ref())
    }
}

impl PropertyGetter for HashMap<String, OwnedValue> {
    fn get(&self, name: &str) -> Option<Option<&OwnedValue>> {
        Some(self.get(name))
    }
}

impl MenuProperties {
    fn apply_properties(&mut self, get: &impl PropertyGetter) {
        macro_rules! get {
            ($field:ident, $name:expr, $ty:ty, $def:expr, $v:ident, $body:block) => {
                if let Some(v) = get.get($name) {
                    match v {
                        None => self.$field = $def,
                        Some(v) => match v.downcast_ref::<$ty>() {
                            Ok($v) => $body,
                            _ => self.$field = $def,
                        },
                    }
                }
            };
        }
        get!(separator, "type", &str, false, v, {
            self.separator = v == "separator"
        });
        get!(enabled, "enabled", bool, true, v, { self.enabled = v });
        get!(visible, "visible", bool, true, v, { self.visible = v });
        get!(icon_name, "icon-name", &str, Default::default(), v, {
            self.icon_name = Arc::new(v.to_string())
        });
        get!(icon_png, "icon-data", Array, Default::default(), v, {
            match v.try_into() {
                Ok(v) => self.icon_png = Arc::new(v),
                _ => self.icon_png = Default::default(),
            }
        });
        get!(toggle_type, "toggle-type", &str, Default::default(), v, {
            self.toggle_type = match v {
                "checkmark" => Some(SniMenuToggleType::Checkmark),
                "radio" => Some(SniMenuToggleType::Radio),
                _ => None,
            };
        });
        get!(toggle_state, "toggle-state", i32, Default::default(), v, {
            self.toggle_state = v == 1;
        });
        get!(children_display, "children-display", &str, false, v, {
            self.children_display = v == "submenu"
        });
        if let Some(v) = get.get("label") {
            'label: {
                let Some(v) = v else {
                    self.label = Default::default();
                    self.access_key = None;
                    break 'label;
                };
                let Ok(s) = v.downcast_ref::<&str>() else {
                    self.label = Default::default();
                    self.access_key = None;
                    break 'label;
                };
                let mut label = String::new();
                let mut last_was_underscore = false;
                for c in s.chars() {
                    if c == '_' {
                        if last_was_underscore {
                            label.push('_');
                            last_was_underscore = false;
                            continue;
                        }
                        last_was_underscore = true;
                        continue;
                    }
                    if last_was_underscore {
                        self.access_key = Some(c);
                        last_was_underscore = false;
                    }
                    label.push(c);
                }
                self.label = Arc::new(label);
            }
        };
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SniMenuToggleType {
    Checkmark,
    Radio,
}

#[derive(Debug, Deserialize, Type, Value, OwnedValue)]
struct MenuLayoutValue {
    menu_id: i32,
    properties: HashMap<String, OwnedValue>,
    children: Vec<OwnedValue>,
}

#[derive(Debug, Clone)]
pub struct MenuTree {
    menu_id: i32,
    properties: MenuProperties,
    children: IndexMap<i32, MenuTree>,
}

impl MenuTree {
    fn merge_properties(&mut self, new: MenuProperties) -> Option<SniMenuPropertiesDelta> {
        let mut prop_delta = SniMenuPropertiesDelta::default();
        let mut any_props_differ = false;
        macro_rules! diff_prop {
            ($name:ident) => {
                if self.properties.$name != new.$name {
                    any_props_differ = true;
                    prop_delta.$name = Some(new.$name.clone());
                    self.properties.$name = new.$name;
                }
            };
        }
        diff_prop!(separator);
        diff_prop!(access_key);
        diff_prop!(label);
        diff_prop!(enabled);
        diff_prop!(visible);
        diff_prop!(icon_name);
        diff_prop!(icon_png);
        diff_prop!(toggle_type);
        diff_prop!(toggle_state);
        diff_prop!(children_display);
        any_props_differ.then_some(prop_delta)
    }

    fn merge(&mut self, new: MenuTree) -> Option<SniMenuDelta> {
        assert_eq!(self.menu_id, new.menu_id);
        let prop_delta = self.merge_properties(new.properties);
        let mut any_child_changed = false;
        let mut children = IndexMap::new();
        self.children.retain(|menu_id, _| {
            let retain = new.children.contains_key(menu_id);
            if !retain {
                any_child_changed = true;
            }
            retain
        });
        for child in new.children.into_values() {
            let menu_id = child.menu_id;
            let delta = match self.children.get_mut(&child.menu_id) {
                None => {
                    self.children.insert(child.menu_id, child.clone());
                    Some(child.into())
                }
                Some(c) => c.merge(child),
            };
            if delta.is_some() {
                any_child_changed = true;
            }
            children.insert(menu_id, delta);
        }
        if prop_delta.is_none() && !any_child_changed {
            return None;
        }
        Some(SniMenuDelta {
            menu_id: self.menu_id,
            properties: prop_delta,
            children: any_child_changed.then_some(children),
        })
    }

    fn merge_nested(
        &mut self,
        menu_id: i32,
        new: &mut Option<MenuTree>,
    ) -> Option<Option<SniMenuDelta>> {
        assert!(new.is_some());
        if self.menu_id == menu_id {
            return Some(self.merge(new.take().unwrap()));
        }
        let mut children = IndexMap::new();
        for c in self.children.values_mut() {
            if new.is_some() {
                if let Some(d) = c.merge_nested(menu_id, new) {
                    match d {
                        Some(d) => {
                            children.insert(c.menu_id, Some(d));
                            continue;
                        }
                        None => return Some(None),
                    }
                }
            }
            children.insert(c.menu_id, None);
        }
        if new.is_none() {
            Some(Some(SniMenuDelta {
                menu_id: self.menu_id,
                properties: None,
                children: Some(children),
            }))
        } else {
            None
        }
    }

    fn merge_property_tree(
        &mut self,
        diff: &AHashMap<i32, AHashMap<String, Option<OwnedValue>>>,
    ) -> Option<SniMenuDelta> {
        let mut prop_delta = None;
        if let Some(diff) = diff.get(&self.menu_id) {
            let mut new = self.properties.clone();
            new.apply_properties(diff);
            prop_delta = self.merge_properties(new);
        }
        let mut children = IndexMap::new();
        let mut any_child_changed = false;
        for c in self.children.values_mut() {
            let d = c.merge_property_tree(diff);
            if d.is_some() {
                any_child_changed = true;
            }
            children.insert(c.menu_id, d);
        }
        if prop_delta.is_none() && !any_child_changed {
            return None;
        }
        Some(SniMenuDelta {
            menu_id: self.menu_id,
            properties: prop_delta,
            children: any_child_changed.then_some(children),
        })
    }
}

#[derive(Default, Clone, Debug, Eq, PartialEq)]
pub struct SniMenuPropertiesDelta {
    pub separator: Option<bool>,
    pub access_key: Option<Option<char>>,
    pub label: Option<Arc<String>>,
    pub enabled: Option<bool>,
    pub visible: Option<bool>,
    pub icon_name: Option<Arc<String>>,
    pub icon_png: Option<Arc<Vec<u8>>>,
    pub toggle_type: Option<Option<SniMenuToggleType>>,
    pub toggle_state: Option<bool>,
    pub children_display: Option<bool>,
}

impl From<MenuProperties> for SniMenuPropertiesDelta {
    fn from(value: MenuProperties) -> Self {
        Self {
            separator: Some(value.separator),
            access_key: Some(value.access_key),
            label: Some(value.label),
            enabled: Some(value.enabled),
            visible: Some(value.visible),
            icon_name: Some(value.icon_name),
            icon_png: Some(value.icon_png),
            toggle_type: Some(value.toggle_type),
            toggle_state: Some(value.toggle_state),
            children_display: Some(value.children_display),
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct SniMenuDelta {
    pub menu_id: i32,
    pub properties: Option<SniMenuPropertiesDelta>,
    pub children: Option<IndexMap<i32, Option<SniMenuDelta>>>,
}

impl From<MenuTree> for SniMenuDelta {
    fn from(value: MenuTree) -> Self {
        Self {
            menu_id: value.menu_id,
            properties: Some(value.properties.into()),
            children: Some(
                value
                    .children
                    .into_values()
                    .map(|v| (v.menu_id, Some(v.into())))
                    .collect(),
            ),
        }
    }
}

impl MenuLayoutValue {
    pub fn parse_tree(mut self) -> MenuTree {
        let mut children = IndexMap::new();
        for child in self.children.drain(..) {
            if let Ok(child) = MenuLayoutValue::try_from(child) {
                children.insert(child.menu_id, child.parse_tree());
            }
        }
        MenuTree {
            menu_id: self.menu_id,
            properties: self.parse_properties(),
            children,
        }
    }

    pub fn parse_properties(&self) -> MenuProperties {
        let mut props = MenuProperties::default();
        props.apply_properties(&self.properties);
        props
    }
}
