mod item;
pub mod protocols;
mod scale;
mod seat;
mod sni_proxy;
mod tray;
mod utils;

#[cfg(feature = "ext-tray")]
use crate::wayland::protocols::ext_tray_v1::ext_tray_v1::ExtTrayV1;
use {
    crate::{
        sni::{MutableProperty, SniItem, SniMenuDelta},
        wayland::{
            item::{Item, Items},
            protocols::{
                cursor_shape_v1::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1,
                ext_tray_v1::ext_tray_item_v1::{ExtTrayItemV1EventHandler, ExtTrayItemV1Ref},
                fractional_scale_v1::{
                    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
                    wp_fractional_scale_v1::{
                        WpFractionalScaleV1EventHandler, WpFractionalScaleV1Ref,
                    },
                },
                jay_tray_v1::{
                    jay_tray_item_v1::{JayTrayItemV1EventHandler, JayTrayItemV1Ref},
                    jay_tray_v1::JayTrayV1,
                },
                viewporter::wp_viewporter::WpViewporter,
                wayland::{
                    wl_buffer::{WlBufferEventHandler, WlBufferRef},
                    wl_callback::{WlCallbackEventHandler, WlCallbackRef},
                    wl_compositor::WlCompositor,
                    wl_display::WlDisplay,
                    wl_pointer::{
                        WlPointerAxis, WlPointerButtonState, WlPointerEventHandler, WlPointerRef,
                    },
                    wl_registry::{WlRegistry, WlRegistryEventHandler, WlRegistryRef},
                    wl_seat::{WlSeat, WlSeatCapability, WlSeatEventHandler, WlSeatRef},
                    wl_shm::WlShm,
                    wl_surface::{WlSurfaceEventHandler, WlSurfaceRef},
                },
                xdg_shell::{
                    xdg_popup::{XdgPopupEventHandler, XdgPopupRef},
                    xdg_positioner::{XdgPositionerAnchor, XdgPositionerGravity},
                    xdg_surface::{XdgSurfaceEventHandler, XdgSurfaceRef},
                    xdg_wm_base::{XdgWmBase, XdgWmBaseEventHandler, XdgWmBaseRef},
                },
            },
            scale::{Logical, Scale},
            seat::Seat,
            sni_proxy::{EventSink, event_stream},
            tray::{
                PopupId, TrayItemId, TraySurfaceId, Trays, item::menu::MenuId,
                protocols::WaylandTray,
            },
        },
    },
    ahash::AHashMap,
    std::{
        convert::Infallible,
        io::{self},
        sync::Arc,
    },
    thiserror::Error,
    tokio::select,
    wl_client::{
        Fixed, Libwayland,
        proxy::{self, OwnedProxy},
    },
};

#[derive(Debug, Error)]
pub enum WaylandError {
    #[error("Could not load libwayland")]
    LoadLibwayland(#[source] io::Error),
    #[error("Could not connect to the compositor")]
    ConnectCompositor(#[source] io::Error),
    #[error("Could not connect to dbus")]
    ConnectDbus(#[source] zbus::Error),
    #[error("Could not wait for events")]
    WaitForEvents(#[source] io::Error),
    #[error("Could not dispatch wayland events")]
    WaylandDispatch(#[source] io::Error),
}

pub async fn run() -> Result<Infallible, WaylandError> {
    let lib = Libwayland::open().map_err(WaylandError::LoadLibwayland)?;
    let conn = lib
        .connect_to_default_display()
        .map_err(WaylandError::ConnectCompositor)?;

    let (_owner, queue) = conn.create_queue_with_data(c"wl-tray-bridge");

    let display = queue.display::<WlDisplay>();
    let registry = display.get_registry();
    proxy::set_event_handler(&registry, WlRegistryHandler);

    let initial_roundtrip = display.sync();
    proxy::set_event_handler(&initial_roundtrip, InitialRoundtrip);

    let dbus = zbus::Connection::session()
        .await
        .map_err(WaylandError::ConnectDbus)?;
    let dbus = bussy::Connection::wrap(&dbus);
    let (sink, mut stream) = event_stream();

    let mut state = State {
        registry,
        singletons_opt: Default::default(),
        singletons: None,
        items: Default::default(),
        trays: Default::default(),
        seats: Default::default(),
        sink: sink.clone(),
        dbus: dbus.connection.clone(),
    };

    loop {
        select! {
            res = queue.wait_for_events() => {
                res.map_err(WaylandError::WaitForEvents)?;
                queue.dispatch_pending(&mut state).map_err(WaylandError::WaylandDispatch)?;
            },
            action = stream.poll() => {
                if let Some(action) = action {
                    action(&mut state);
                }
            }
        }
    }
}

#[derive(Default)]
struct SingletonsOpt {
    wl_compositor: Option<WlCompositor>,
    wl_shm: Option<WlShm>,
    wp_viewporter: Option<WpViewporter>,
    wp_fractional_scale_manager_v1: Option<WpFractionalScaleManagerV1>,
    wp_cursor_shape_manager_v1: Option<WpCursorShapeManagerV1>,
    xdg_wm_base: Option<XdgWmBase>,
    xdg_wm_base_version: u32,
}

struct Singletons {
    sink: EventSink,
    wl_compositor: WlCompositor,
    wl_shm: WlShm,
    wp_viewporter: WpViewporter,
    wp_cursor_shape_manager_v1: WpCursorShapeManagerV1,
    xdg_wm_base: XdgWmBase,
    xdg_wm_base_version: u32,
    wp_fractional_scale_manager_v1: Option<WpFractionalScaleManagerV1>,
}

struct State {
    registry: WlRegistry,
    singletons_opt: SingletonsOpt,
    singletons: Option<Singletons>,
    items: Items,
    trays: Trays,
    seats: AHashMap<u32, Seat>,
    sink: EventSink,
    dbus: Arc<bussy::Connection>,
}

fn s(s: &Option<Singletons>) -> &Singletons {
    match s {
        Some(s) => s,
        None => {
            panic!("Singletons are not initialized");
        }
    }
}

impl State {
    fn handle_new_sni_item(&mut self, sni: Arc<SniItem>) {
        let mut item = Item {
            sni: sni.clone(),
            props: sni.properties(),
            icon: Default::default(),
            attention_icon: Default::default(),
            menu: Default::default(),
        };
        if let Some(s) = &self.singletons {
            item.initialize();
            self.trays.add_item(s, &item);
        }
        self.items.items.insert(sni.id(), item);
    }

    fn handle_sni_item_prop_changed(&mut self, sni: &Arc<SniItem>, prop: MutableProperty) {
        let Some(item) = self.items.items.get_mut(&sni.id()) else {
            return;
        };
        item.props = sni.properties();
        match prop {
            MutableProperty::Icon => item.update_icon(),
            MutableProperty::AttentionIcon => item.update_attention_icon(),
            _ => {}
        }
        self.trays
            .handle_item_prop_changed(s(&self.singletons), item, prop);
    }

    fn handle_sni_item_removed(&mut self, item: &Arc<SniItem>) {
        self.items.items.remove(&item.id());
        self.trays.handle_item_removed(item.id());
    }

    fn handle_sni_menu_changed(&mut self, item: &Arc<SniItem>, delta: SniMenuDelta) {
        let Some(item) = self.items.items.get_mut(&item.id()) else {
            return;
        };
        item.menu.apply_delta(&delta);
        self.trays
            .handle_menu_changed(s(&self.singletons), item, &delta);
    }

    fn handle_seat_timeout(&mut self, seat_name: u32, timeout_id: usize) {
        let Some(seat) = self.seats.get_mut(&seat_name) else {
            return;
        };
        seat.handle_timeout(
            &self.items,
            s(&self.singletons),
            &mut self.trays,
            timeout_id,
        );
    }

    fn open_menu(&mut self, seat_name: u32, tray_item: TrayItemId, menu: MenuId) {
        let Some(seat) = self.seats.get(&seat_name) else {
            return;
        };
        self.trays
            .open_menu(seat, &self.items, s(&self.singletons), tray_item, menu);
    }

    fn open_root_menu(&mut self, seat_name: u32, tray_item: TrayItemId) {
        let Some(seat) = self.seats.get(&seat_name) else {
            return;
        };
        self.trays
            .open_root_menu(seat, &self.items, s(&self.singletons), tray_item);
    }

    fn bind_tray<T>(&mut self, registry: &WlRegistry, name: u32)
    where
        T: 'static,
        T: WaylandTray,
        T: OwnedProxy,
    {
        let tray = registry.bind::<T>(name, 1);
        let tray = self.trays.create_tray(Box::new(tray), name);
        if let Some(tray) = tray {
            if let Some(s) = &self.singletons {
                for item in self.items.items.values() {
                    tray.add_item(s, item);
                }
            }
        }
    }
}

struct WlRegistryHandler;

impl WlRegistryEventHandler for WlRegistryHandler {
    type Data = State;

    fn global(
        &self,
        state: &mut Self::Data,
        _slf: &WlRegistryRef,
        name: u32,
        interface: &str,
        version: u32,
    ) {
        let registry = &state.registry;
        match interface {
            WlCompositor::INTERFACE => {
                state.singletons_opt.wl_compositor = Some(registry.bind(name, 4));
            }
            WlShm::INTERFACE => {
                state.singletons_opt.wl_shm = Some(registry.bind(name, 1));
            }
            WpViewporter::INTERFACE => {
                state.singletons_opt.wp_viewporter = Some(registry.bind(name, 1));
            }
            WpFractionalScaleManagerV1::INTERFACE => {
                state.singletons_opt.wp_fractional_scale_manager_v1 = Some(registry.bind(name, 1));
            }
            XdgWmBase::INTERFACE => {
                let version = version.min(6);
                let xdg_wm_base = registry.bind(name, version);
                proxy::set_event_handler(&xdg_wm_base, XdgWmBaseHandler);
                state.singletons_opt.xdg_wm_base = Some(xdg_wm_base);
                state.singletons_opt.xdg_wm_base_version = version;
            }
            WpCursorShapeManagerV1::INTERFACE => {
                state.singletons_opt.wp_cursor_shape_manager_v1 = Some(registry.bind(name, 1));
            }
            #[cfg(feature = "ext-tray")]
            ExtTrayV1::INTERFACE => {
                state.bind_tray::<ExtTrayV1>(&registry.clone(), name);
            }
            JayTrayV1::INTERFACE => {
                state.bind_tray::<JayTrayV1>(&registry.clone(), name);
            }
            WlSeat::INTERFACE => {
                let seat = registry.bind(name, version.min(8));
                proxy::set_event_handler(&seat, WlSeatHandler { name });
                state.seats.insert(name, Seat::new(seat, name));
            }
            _ => {}
        }
    }

    fn global_remove(&self, state: &mut Self::Data, _slf: &WlRegistryRef, name: u32) {
        if let Some(mut seat) = state.seats.remove(&name) {
            seat.handle_remove(&mut state.trays);
        }
        state.trays.handle_global_remove(name);
    }
}

struct InitialRoundtrip;

impl WlCallbackEventHandler for InitialRoundtrip {
    type Data = State;

    fn done(&self, state: &mut Self::Data, _slf: &WlCallbackRef, _callback_data: u32) {
        macro_rules! get {
            ($name:ident) => {{
                match state.singletons_opt.$name.take() {
                    Some(s) => s,
                    _ => {
                        log::error!("Compositor does not support {}", stringify!($name));
                        std::process::exit(1);
                    }
                }
            }};
        }
        let singletons = Singletons {
            sink: state.sink.clone(),
            wl_compositor: get!(wl_compositor),
            wl_shm: get!(wl_shm),
            wp_viewporter: get!(wp_viewporter),
            wp_cursor_shape_manager_v1: get!(wp_cursor_shape_manager_v1),
            xdg_wm_base: get!(xdg_wm_base),
            xdg_wm_base_version: state.singletons_opt.xdg_wm_base_version,
            wp_fractional_scale_manager_v1: state
                .singletons_opt
                .wp_fractional_scale_manager_v1
                .take(),
        };
        for item in state.items.items.values_mut() {
            item.initialize();
            state.trays.add_item(&singletons, item);
        }
        state.singletons = Some(singletons);
        sni_proxy::spawn(&state.dbus, &state.sink);
    }
}

struct XdgWmBaseHandler;

impl XdgWmBaseEventHandler for XdgWmBaseHandler {
    type Data = State;

    fn ping(&self, _data: &mut Self::Data, slf: &XdgWmBaseRef, serial: u32) {
        slf.pong(serial);
    }
}

struct ExtTrayItemV1Handler {
    id: TrayItemId,
}

impl ExtTrayItemV1EventHandler for ExtTrayItemV1Handler {
    type Data = State;

    fn configure_size(
        &self,
        state: &mut Self::Data,
        _slf: &ExtTrayItemV1Ref,
        width: i32,
        height: i32,
    ) {
        let Some(tray_item) = state.trays.get_item_mut(self.id) else {
            return;
        };
        tray_item.configure_size(Logical(width, height));
    }

    fn preferred_anchor(
        &self,
        state: &mut Self::Data,
        _slf: &ExtTrayItemV1Ref,
        anchor: XdgPositionerAnchor,
    ) {
        let Some(tray_item) = state.trays.get_item_mut(self.id) else {
            return;
        };
        tray_item.set_preferred_anchor(anchor);
    }

    fn preferred_gravity(
        &self,
        state: &mut Self::Data,
        _slf: &ExtTrayItemV1Ref,
        gravity: XdgPositionerGravity,
    ) {
        let Some(tray_item) = state.trays.get_item_mut(self.id) else {
            return;
        };
        tray_item.set_preferred_gravity(gravity);
    }

    fn configure(&self, state: &mut Self::Data, _slf: &ExtTrayItemV1Ref, serial: u32) {
        let Some(tray_item) = state.trays.get_item_mut(self.id) else {
            return;
        };
        let Some(item) = state.items.items.get(&self.id.item) else {
            return;
        };
        tray_item.configure(Some(serial), s(&state.singletons), item);
    }
}

struct JayTrayItemV1Handler {
    id: TrayItemId,
}

impl JayTrayItemV1EventHandler for JayTrayItemV1Handler {
    type Data = State;

    fn configure_size(
        &self,
        state: &mut Self::Data,
        _slf: &JayTrayItemV1Ref,
        width: i32,
        height: i32,
    ) {
        let Some(tray_item) = state.trays.get_item_mut(self.id) else {
            return;
        };
        tray_item.configure_size(Logical(width, height));
    }

    fn preferred_anchor(
        &self,
        state: &mut Self::Data,
        _slf: &JayTrayItemV1Ref,
        anchor: XdgPositionerAnchor,
    ) {
        let Some(tray_item) = state.trays.get_item_mut(self.id) else {
            return;
        };
        tray_item.set_preferred_anchor(anchor);
    }

    fn preferred_gravity(
        &self,
        state: &mut Self::Data,
        _slf: &JayTrayItemV1Ref,
        gravity: XdgPositionerGravity,
    ) {
        let Some(tray_item) = state.trays.get_item_mut(self.id) else {
            return;
        };
        tray_item.set_preferred_gravity(gravity);
    }

    fn configure(&self, state: &mut Self::Data, _slf: &JayTrayItemV1Ref, serial: u32) {
        let Some(tray_item) = state.trays.get_item_mut(self.id) else {
            return;
        };
        let Some(item) = state.items.items.get(&self.id.item) else {
            return;
        };
        tray_item.configure(Some(serial), s(&state.singletons), item);
    }
}

struct WlSeatHandler {
    name: u32,
}

impl WlSeatEventHandler for WlSeatHandler {
    type Data = State;

    fn capabilities(
        &self,
        state: &mut Self::Data,
        _slf: &WlSeatRef,
        capabilities: WlSeatCapability,
    ) {
        let Some(seat) = state.seats.get_mut(&self.name) else {
            return;
        };
        seat.update_capabilities(s(&state.singletons), &mut state.trays, capabilities);
    }
}

struct WlPointerHandler {
    name: u32,
}

impl WlPointerEventHandler for WlPointerHandler {
    type Data = State;

    fn enter(
        &self,
        state: &mut Self::Data,
        _slf: &WlPointerRef,
        serial: u32,
        surface: Option<&WlSurfaceRef>,
        surface_x: Fixed,
        surface_y: Fixed,
    ) {
        let Some(surface) = surface else {
            return;
        };
        let Some(seat) = state.seats.get_mut(&self.name) else {
            return;
        };
        seat.handle_pointer_enter(
            &state.items,
            s(&state.singletons),
            &mut state.trays,
            surface,
            surface_x.to_i32_floor(),
            surface_y.to_i32_floor(),
            serial,
        );
    }

    fn leave(
        &self,
        state: &mut Self::Data,
        _slf: &WlPointerRef,
        _serial: u32,
        _surface: Option<&WlSurfaceRef>,
    ) {
        let Some(seat) = state.seats.get_mut(&self.name) else {
            return;
        };
        seat.handle_pointer_leave(&mut state.trays);
    }

    fn motion(
        &self,
        state: &mut Self::Data,
        _slf: &WlPointerRef,
        _time: u32,
        surface_x: Fixed,
        surface_y: Fixed,
    ) {
        let Some(seat) = state.seats.get_mut(&self.name) else {
            return;
        };
        seat.handle_pointer_motion(
            &state.items,
            s(&state.singletons),
            &mut state.trays,
            surface_x.to_i32_floor(),
            surface_y.to_i32_floor(),
            None,
        );
    }

    fn button(
        &self,
        state: &mut Self::Data,
        _slf: &WlPointerRef,
        serial: u32,
        _time: u32,
        button: u32,
        button_state: WlPointerButtonState,
    ) {
        if button_state != WlPointerButtonState::PRESSED {
            return;
        }
        let Some(seat) = state.seats.get_mut(&self.name) else {
            return;
        };
        seat.handle_button_pressed(
            s(&state.singletons),
            &mut state.trays,
            &mut state.items,
            button,
            serial,
        );
    }

    fn axis_discrete(
        &self,
        state: &mut Self::Data,
        _slf: &WlPointerRef,
        axis: WlPointerAxis,
        discrete: i32,
    ) {
        let Some(seat) = state.seats.get_mut(&self.name) else {
            return;
        };
        seat.handle_axis_value120(&mut state.trays, axis, discrete * 120);
    }

    fn axis_value120(
        &self,
        state: &mut Self::Data,
        _slf: &WlPointerRef,
        axis: WlPointerAxis,
        value120: i32,
    ) {
        let Some(seat) = state.seats.get_mut(&self.name) else {
            return;
        };
        seat.handle_axis_value120(&mut state.trays, axis, value120);
    }
}

struct XdgSurfaceHandler {
    id: PopupId,
}

impl XdgSurfaceEventHandler for XdgSurfaceHandler {
    type Data = State;

    fn configure(&self, state: &mut Self::Data, _slf: &XdgSurfaceRef, serial: u32) {
        state.trays.handle_popup_configured(self.id, serial);
    }
}

struct XdgPopupHandler {
    id: PopupId,
}

impl XdgPopupEventHandler for XdgPopupHandler {
    type Data = State;

    fn popup_done(&self, state: &mut Self::Data, _slf: &XdgPopupRef) {
        state.trays.handle_popup_done(self.id);
    }

    fn repositioned(&self, state: &mut Self::Data, _slf: &XdgPopupRef, token: u32) {
        state.trays.handle_popup_repositioned(self.id, token);
    }
}

struct WlBufferHandler {
    id: TraySurfaceId,
}

impl WlBufferEventHandler for WlBufferHandler {
    type Data = State;

    fn release(&self, state: &mut Self::Data, slf: &WlBufferRef) {
        state.trays.handle_buffer_released(self.id, slf);
    }
}

struct WpFractionalScaleV1Handler {
    id: TrayItemId,
}

impl WpFractionalScaleV1EventHandler for WpFractionalScaleV1Handler {
    type Data = State;

    fn preferred_scale(&self, state: &mut Self::Data, _slf: &WpFractionalScaleV1Ref, scale: u32) {
        let scale = Scale(scale as _);
        state
            .trays
            .handle_scale(&state.items, s(&state.singletons), self.id, scale);
    }
}

struct WlSurfaceHandler {
    id: TrayItemId,
}

impl WlSurfaceEventHandler for WlSurfaceHandler {
    type Data = State;

    fn preferred_buffer_scale(&self, state: &mut Self::Data, _slf: &WlSurfaceRef, factor: i32) {
        let s = s(&state.singletons);
        if s.wp_fractional_scale_manager_v1.is_none() {
            let scale = Scale(factor * 120);
            state.trays.handle_scale(&state.items, s, self.id, scale);
        }
    }
}
