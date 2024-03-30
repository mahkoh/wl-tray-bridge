use {
    crate::{
        sni::{MutableProperty, SniItemId, SniMenuDelta},
        wayland::{
            item::Items,
            scale::{Logical, Scale},
            seat::{MotionResult, Seat},
            tray::{
                ext_tray_v1::client::ext_tray_v1::ExtTrayV1,
                item::{
                    menu::{MenuId, MenuInstance},
                    TrayItem,
                },
            },
            Item, Singletons,
        },
    },
    ahash::AHashMap,
    wayland_client::protocol::{wl_buffer::WlBuffer, wl_pointer::Axis, wl_surface::WlSurface},
};

pub mod item;

pub mod ext_tray_v1 {
    pub mod client {
        use {
            self::__interfaces::*,
            wayland_client::{self, protocol::*},
            wayland_protocols::xdg::shell::client::*,
        };
        pub mod __interfaces {
            use {
                wayland_client::protocol::__interfaces::*,
                wayland_protocols::xdg::shell::client::__interfaces::*,
            };
            wayland_scanner::generate_interfaces!("tray-v1.xml");
        }
        wayland_scanner::generate_client_code!("tray-v1.xml");
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PopupId {
    pub tray_item: TrayItemId,
    pub ty: PopupIdType,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PopupIdType {
    Tooltip,
    MenuId(MenuId),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct TrayItemId {
    pub tray: u32,
    pub item: SniItemId,
}

pub struct Tray {
    name: u32,
    tray: ExtTrayV1,
    items: AHashMap<SniItemId, TrayItem>,
}

#[derive(Default)]
pub struct Trays {
    trays: AHashMap<u32, Tray>,
}

impl Trays {
    pub fn create_tray(&mut self, tray: ExtTrayV1, name: u32) -> &mut Tray {
        self.trays.entry(name).or_insert(Tray {
            name,
            tray,
            items: Default::default(),
        })
    }

    pub fn add_item(&mut self, singletons: &Singletons, item: &Item) {
        for tray in self.trays.values_mut() {
            tray.add_item(singletons, item);
        }
    }

    pub fn handle_item_prop_changed(&mut self, s: &Singletons, item: &Item, prop: MutableProperty) {
        for tray in self.trays.values_mut() {
            if let Some(tray_item) = tray.items.get_mut(&item.sni.id()) {
                tray_item.handle_item_prop_changed(s, item, prop);
            }
        }
    }

    pub fn handle_item_removed(&mut self, item: SniItemId) {
        for tray in self.trays.values_mut() {
            tray.items.remove(&item);
        }
    }

    pub fn get_item_mut(&mut self, id: TrayItemId) -> Option<&mut TrayItem> {
        self.trays.get_mut(&id.tray)?.items.get_mut(&id.item)
    }

    pub fn find_surface(&self, surface: &WlSurface) -> Option<TraySurfaceId> {
        for tray in self.trays.values() {
            if let Some(s) = tray.find_surface(surface) {
                return Some(s);
            }
        }
        None
    }

    #[expect(clippy::too_many_arguments)]
    pub fn handle_motion(
        &mut self,
        seat: &Seat,
        serial: Option<u32>,
        items: &Items,
        s: &Singletons,
        surface: TraySurfaceId,
        x: i32,
        y: i32,
    ) -> MotionResult {
        let Some(item) = items.items.get(&surface.item.item) else {
            return MotionResult::None;
        };
        let Some(tray_item) = self.get_item_mut(surface.item) else {
            return MotionResult::None;
        };
        tray_item.handle_motion(seat, serial, item, s, surface.menu, x, y)
    }

    pub fn handle_leave(&mut self, seat: &Seat, surface: TraySurfaceId) {
        let Some(item) = self.get_item_mut(surface.item) else {
            return;
        };
        item.handle_leave(seat, surface.menu)
    }

    pub fn handle_timeout(
        &mut self,
        seat: &Seat,
        items: &Items,
        s: &Singletons,
        surface: TraySurfaceId,
        menu_id: Option<MenuId>,
    ) {
        let Some(item) = self.get_item_mut(surface.item) else {
            return;
        };
        item.handle_timeout(seat, items, s, surface.menu, menu_id)
    }

    pub fn handle_scroll(&mut self, surface: TraySurfaceId, axis: Axis, steps: i32) {
        let Some(item) = self.get_item_mut(surface.item) else {
            return;
        };
        item.handle_scroll(surface.menu, axis, steps);
    }

    pub fn handle_menu_changed(&mut self, s: &Singletons, item: &Item, delta: &SniMenuDelta) {
        for tray in self.trays.values_mut() {
            if let Some(tray_item) = tray.items.get_mut(&item.sni.id()) {
                if let Some(menu) = &mut tray_item.menu {
                    if !menu.apply_delta(&item.menu, delta, s) {
                        tray_item.menu = None;
                    }
                }
            }
        }
    }

    pub fn handle_popup_repositioned(&mut self, id: PopupId, token: u32) {
        let Some(item) = self.get_item_mut(id.tray_item) else {
            return;
        };
        item.handle_popup_repositioned(id.ty, token);
    }

    pub fn handle_buffer_released(&mut self, id: TraySurfaceId, buffer: &WlBuffer) {
        let Some(item) = self.get_item_mut(id.item) else {
            return;
        };
        item.handle_buffer_released(id.menu, buffer);
    }

    pub fn handle_scale(&mut self, items: &Items, s: &Singletons, id: TrayItemId, scale: Scale) {
        let Some(item) = items.items.get(&id.item) else {
            return;
        };
        let Some(tray_item) = self.get_item_mut(id) else {
            return;
        };
        tray_item.handle_scale(s, item, scale);
    }

    pub fn handle_popup_configured(&mut self, id: PopupId, serial: u32) {
        let Some(item) = self.get_item_mut(id.tray_item) else {
            return;
        };
        item.handle_popup_configure(id.ty, serial);
    }

    pub fn handle_popup_done(&mut self, id: PopupId) {
        let Some(item) = self.get_item_mut(id.tray_item) else {
            return;
        };
        item.handle_popup_done(id.ty);
    }

    pub fn handle_button(
        &mut self,
        seat: &Seat,
        serial: u32,
        id: TraySurfaceId,
        s: &Singletons,
        item: &Item,
        button: u32,
    ) {
        let Some(tray) = self.trays.get_mut(&id.item.tray) else {
            return;
        };
        for tray_item in tray.items.values_mut() {
            if tray_item.id == id.item {
                tray_item.handle_button(seat, serial, id.menu, s, item, button);
            } else {
                tray_item.menu = None;
            }
        }
    }

    pub fn open_menu(
        &mut self,
        seat: &Seat,
        items: &Items,
        s: &Singletons,
        id: TrayItemId,
        menu_id: MenuId,
    ) {
        let Some(item) = items.items.get(&id.item) else {
            return;
        };
        let Some(tray_item) = self.get_item_mut(id) else {
            return;
        };
        let Some(&serial) = tray_item.seat_serials.get(&seat.name()) else {
            return;
        };
        if menu_id == 0 {
            tray_item.menu = MenuInstance::new(seat, serial, tray_item, &item.menu, s);
        } else {
            let Some(menu) = &mut tray_item.menu else {
                return;
            };
            menu.open_child(&item.menu, s, menu_id);
        }
    }

    pub fn open_root_menu(&mut self, seat: &Seat, items: &Items, s: &Singletons, id: TrayItemId) {
        let Some(item) = items.items.get(&id.item) else {
            return;
        };
        let Some(tray_item) = self.get_item_mut(id) else {
            return;
        };
        tray_item.open_root_menu(seat, s, item);
    }

    pub fn handle_global_remove(&mut self, name: u32) {
        self.trays.remove(&name);
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TraySurfaceId {
    pub item: TrayItemId,
    pub menu: Option<MenuId>,
}

impl Tray {
    pub fn find_surface(&self, surface: &WlSurface) -> Option<TraySurfaceId> {
        for item in self.items.values() {
            if let Some(s) = item.find_surface(surface) {
                return Some(s);
            }
        }
        None
    }

    pub fn add_item(&mut self, s: &Singletons, item: &Item) {
        let id = TrayItemId {
            tray: self.name,
            item: item.sni.id(),
        };
        let surface = s.wl_compositor.create_surface(&s.qh, id);
        let fractional_scale = s
            .wp_fractional_scale_manager_v1
            .as_ref()
            .map(|m| m.get_fractional_scale(&surface, &s.qh, id));
        let viewport = s.wp_viewporter.get_viewport(&surface, &s.qh, ());
        let ext_item = self.tray.get_tray_item(&surface, &s.qh, id);
        self.items.insert(
            item.sni.id(),
            TrayItem {
                id,
                surface,
                viewport,
                item: ext_item,
                size: Logical(0, 0),
                sni: item.sni.clone(),
                tooltip: None,
                scale: Scale(120),
                buffers: Default::default(),
                menu: None,
                seat_serials: Default::default(),
                seat_positions: Default::default(),
                current_activation: None,
                fractional_scale,
            },
        );
    }
}
