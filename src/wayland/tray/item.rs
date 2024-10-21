use {
    crate::{
        settings::{self},
        sni::{MutableProperty, SniItem},
        wayland::{
            item::Items,
            scale::{Logical, Scale},
            seat::{MotionResult, MotionTimeoutTarget, Seat},
            tray::{
                item::{
                    icon::BufferIcon,
                    menu::{MenuId, MenuInstance},
                    tooltip::{create_tooltip, Tooltip},
                },
                protocols::WaylandTrayItem,
                PopupId, PopupIdType, TraySurfaceId,
            },
            Item, Singletons, TrayItemId,
        },
    },
    ahash::AHashMap,
    bussy::Call,
    error_reporter::Report,
    isnt::std_1::string::IsntStringExt,
    std::{sync::Arc, time::Duration},
    wayland_client::protocol::{wl_buffer::WlBuffer, wl_pointer::Axis, wl_surface::WlSurface},
    wayland_protocols::{
        wp::{
            fractional_scale::v1::client::wp_fractional_scale_v1::WpFractionalScaleV1,
            viewporter::client::wp_viewport::WpViewport,
        },
        xdg::shell::client::{
            xdg_popup::XdgPopup,
            xdg_positioner::{Anchor, ConstraintAdjustment, Gravity},
            xdg_surface::XdgSurface,
        },
    },
};

pub mod icon;
pub mod menu;
pub mod tooltip;

#[derive(Default)]
pub struct TrayItemPending {
    size: Option<Logical>,
    preferred_anchor: Option<Anchor>,
    preferred_gravity: Option<Gravity>,
}

pub struct TrayItem {
    pub(super) id: TrayItemId,
    pub(super) surface: WlSurface,
    pub(super) fractional_scale: Option<WpFractionalScaleV1>,
    pub(super) sni: Arc<SniItem>,
    pub(super) viewport: WpViewport,
    pub(super) item: Box<dyn WaylandTrayItem>,
    pub(super) pending: TrayItemPending,
    pub(super) size: Logical,
    pub(super) preferred_anchor: Anchor,
    pub(super) preferred_gravity: Gravity,
    pub(super) tooltip: Option<TrayItemPopup>,
    pub(super) scale: Scale,
    pub(super) buffers: BufferIcon,
    pub(super) menu: Option<MenuInstance>,
    pub(super) seat_serials: AHashMap<u32, u32>,
    pub(super) seat_positions: AHashMap<u32, (i32, i32)>,
    pub(super) current_activation: Option<Call>,
}

impl Drop for TrayItem {
    fn drop(&mut self) {
        self.menu = None;
        self.tooltip = None;
        self.item.destroy();
        self.viewport.destroy();
        if let Some(fs) = self.fractional_scale.take() {
            fs.destroy();
        }
        self.surface.destroy();
    }
}

pub struct TrayItemPopup {
    tooltip: Tooltip,
    xdg_surface: XdgSurface,
    xdg_popup: XdgPopup,
}

impl Drop for TrayItemPopup {
    fn drop(&mut self) {
        self.xdg_popup.destroy();
        self.xdg_surface.destroy();
    }
}

impl TrayItem {
    pub fn configure_size(&mut self, size: Logical) {
        self.pending.size = Some(size);
    }

    pub fn set_preferred_anchor(&mut self, anchor: Anchor) {
        self.pending.preferred_anchor = Some(anchor);
    }

    pub fn set_preferred_gravity(&mut self, gravity: Gravity) {
        self.pending.preferred_gravity = Some(gravity);
    }

    pub fn configure(&mut self, serial: Option<u32>, singletons: &Singletons, item: &Item) {
        if let Some(serial) = serial {
            self.item.ack_configure(serial);
            macro_rules! apply {
                ($name:ident) => {
                    if let Some(v) = self.pending.$name.take() {
                        self.$name = v;
                    }
                };
            }
            apply!(size);
            apply!(preferred_anchor);
            apply!(preferred_gravity);
        }
        if self.size.0 == 0 || self.size.1 == 0 {
            return;
        }
        self.buffers.update(
            match item.props.status.as_ref().map(|v| &***v) == Some("NeedsAttention") {
                true => &item.attention_icon,
                false => &item.icon,
            },
            self.size.to_physical(self.scale).size(),
            self.scale.round_up(),
            &settings::get().theme,
            &settings::get().icon.color,
            singletons,
        );
        let buffer = self.buffers.get();
        self.viewport.set_destination(self.size.0, self.size.1);
        self.surface.attach(buffer.map(|b| &b.0.buffer), 0, 0);
        self.surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
        self.surface.commit();
    }

    pub fn handle_button(
        &mut self,
        seat: &Seat,
        serial: u32,
        menu: Option<MenuId>,
        s: &Singletons,
        item: &Item,
        button: u32,
    ) {
        self.tooltip = None;
        self.seat_serials.insert(seat.name(), serial);
        if let Some(menu_id) = menu {
            if let Some(menu) = &mut self.menu {
                #[expect(clippy::collapsible_if)]
                if menu.handle_button(seat, &item.menu, menu_id) {
                    if !settings::get().keep_open {
                        self.menu = None;
                    }
                }
            }
        } else {
            const BTN_LEFT: u32 = 0x110;
            const BTN_RIGHT: u32 = 0x111;
            const BTN_MIDDLE: u32 = 0x112;
            let had_menu = self.menu.take().is_some();
            if button == BTN_LEFT || button == BTN_MIDDLE {
                let sink = s.sink.clone();
                let id = self.id;
                let seat_name = seat.name();
                let cb = move |ok: bool| {
                    if !ok && !had_menu {
                        sink.send(move |state| {
                            state.open_root_menu(seat_name, id);
                        });
                    }
                };
                let activation = if button == BTN_LEFT {
                    self.sni.activate(cb)
                } else {
                    self.sni.secondary_activate(cb)
                };
                self.current_activation = Some(activation);
                return;
            }
            if button == BTN_RIGHT && !had_menu {
                self.open_root_menu(seat, s, item);
            }
        }
    }

    pub fn open_root_menu(&mut self, seat: &Seat, s: &Singletons, item: &Item) {
        let Some(&serial) = self.seat_serials.get(&seat.name()) else {
            return;
        };
        let id = self.id;
        let sink = s.sink.clone();
        let seat_name = seat.name();
        let call = self.sni.open_menu(
            0,
            Box::new(move || {
                sink.send(move |state| {
                    state.open_menu(seat_name, id, 0);
                });
            }),
        );
        if call.is_none() {
            self.menu = MenuInstance::new(seat, serial, self, &item.menu, s);
        }
        self.current_activation = call;
    }

    pub fn find_surface(&self, surface: &WlSurface) -> Option<TraySurfaceId> {
        if &self.surface == surface {
            return Some(TraySurfaceId {
                item: self.id,
                menu: None,
            });
        }
        if let Some(menu) = &self.menu {
            return menu.find_surface(surface);
        }
        None
    }

    #[expect(clippy::too_many_arguments)]
    pub fn handle_motion(
        &mut self,
        seat: &Seat,
        serial: Option<u32>,
        item: &Item,
        s: &Singletons,
        menu: Option<MenuId>,
        x: i32,
        y: i32,
    ) -> MotionResult {
        if let Some(serial) = serial {
            self.seat_serials.insert(seat.name(), serial);
        }
        if let Some(menu_id) = menu {
            let Some(menu) = &mut self.menu else {
                return MotionResult::None;
            };
            menu.handle_motion(seat, &item.menu, s, menu_id, x, y)
        } else {
            self.seat_positions.insert(seat.name(), (x, y));
            MotionResult::ContinueTimeout {
                timeout: Duration::from_secs(1),
                target: MotionTimeoutTarget { menu_id: None },
            }
        }
    }

    pub fn handle_leave(&mut self, seat: &Seat, menu: Option<MenuId>) {
        self.tooltip = None;
        self.seat_serials.remove(&seat.name());
        self.seat_positions.remove(&seat.name());
        if let Some(id) = menu {
            if let Some(menu) = &mut self.menu {
                menu.handle_leave(seat, id);
            }
        }
    }

    pub fn handle_timeout(
        &mut self,
        seat: &Seat,
        items: &Items,
        s: &Singletons,
        menu: Option<MenuId>,
        sub_menu_id: Option<MenuId>,
    ) {
        if let Some(menu_id) = menu {
            let Some(menu) = &mut self.menu else {
                return;
            };
            menu.handle_timeout(seat, items, s, menu_id, sub_menu_id);
        } else {
            if self.menu.is_some() {
                return;
            }
            let Some(&serial) = self.seat_serials.get(&seat.name()) else {
                return;
            };
            let Some((x, y)) = self.seat_positions.get(&seat.name()).copied() else {
                return;
            };
            let Some(item) = items.items.get(&self.id.item) else {
                return;
            };
            let title = 'title: {
                if let Some(tooltip) = &item.props.tooltip {
                    if tooltip.title.is_not_empty() {
                        break 'title &*tooltip.title;
                    }
                }
                if let Some(title) = &item.props.title {
                    if title.is_not_empty() {
                        break 'title title;
                    }
                };
                return;
            };
            let id = PopupId {
                tray_item: self.id,
                ty: PopupIdType::Tooltip,
            };
            let tooltip = match create_tooltip(s, self.scale, title) {
                Ok(t) => t,
                Err(e) => {
                    log::error!("Could not create tooltip: {}", Report::new(e));
                    return;
                }
            };
            let positioner = s.xdg_wm_base.create_positioner(&s.qh, ());
            positioner.set_size(tooltip.log_size.0, tooltip.log_size.1);
            positioner.set_anchor_rect(x, y, 1, 1);
            positioner.set_anchor(Anchor::BottomLeft);
            positioner.set_gravity(Gravity::BottomLeft);
            positioner.set_offset(-2, 2);
            positioner.set_constraint_adjustment(ConstraintAdjustment::all());
            let xdg = s.xdg_wm_base.get_xdg_surface(&tooltip.surface, &s.qh, id);
            let popup = xdg.get_popup(None, &positioner, &s.qh, id);
            positioner.destroy();
            self.item.get_popup(&popup, seat.wl_seat(), serial);
            tooltip.surface.commit();
            self.tooltip = Some(TrayItemPopup {
                tooltip,
                xdg_surface: xdg,
                xdg_popup: popup,
            });
        }
    }

    pub fn handle_scroll(&mut self, menu: Option<MenuId>, axis: Axis, steps: i32) {
        if menu.is_none() {
            self.sni.scroll(steps, axis);
        }
    }

    pub fn handle_popup_repositioned(&mut self, ty: PopupIdType, token: u32) {
        match ty {
            PopupIdType::Tooltip => {
                // nothing
            }
            PopupIdType::MenuId(id) => {
                if let Some(menu) = &mut self.menu {
                    menu.repositioned(id, token);
                }
            }
        }
    }

    pub fn handle_buffer_released(&mut self, menu: Option<MenuId>, buffer: &WlBuffer) {
        let Some(menu_id) = menu else {
            return;
        };
        let Some(menu) = &mut self.menu else {
            return;
        };
        menu.handle_buffer_released(menu_id, buffer);
    }

    pub fn handle_scale(&mut self, s: &Singletons, item: &Item, scale: Scale) {
        self.scale = scale;
        self.tooltip = None;
        self.menu = None;
        self.configure(None, s, item);
    }

    pub fn handle_item_prop_changed(&mut self, s: &Singletons, item: &Item, prop: MutableProperty) {
        match prop {
            MutableProperty::Title => self.tooltip = None,
            MutableProperty::Icon | MutableProperty::AttentionIcon | MutableProperty::Status => {
                self.configure(None, s, item);
            }
            _ => {}
        }
    }

    pub fn handle_popup_configure(&mut self, ty: PopupIdType, serial: u32) {
        match ty {
            PopupIdType::Tooltip => {
                if let Some(tt) = &self.tooltip {
                    tt.xdg_surface.ack_configure(serial);
                    tt.tooltip
                        .viewport
                        .set_destination(tt.tooltip.log_size.0, tt.tooltip.log_size.1);
                    tt.tooltip.surface.attach(Some(&tt.tooltip.buffer), 0, 0);
                    tt.tooltip.surface.commit();
                }
            }
            PopupIdType::MenuId(id) => {
                if let Some(menu) = &mut self.menu {
                    menu.configured(id, serial);
                }
            }
        }
    }

    pub fn handle_popup_done(&mut self, ty: PopupIdType) {
        match ty {
            PopupIdType::Tooltip => {
                self.tooltip = None;
            }
            PopupIdType::MenuId(id) => {
                if let Some(menu) = &mut self.menu {
                    if !menu.popup_done(id) {
                        self.menu = None;
                    }
                }
            }
        }
    }
}
