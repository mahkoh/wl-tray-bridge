mod item;
mod scale;
mod seat;
mod sni_proxy;
mod tray;
mod utils;

use {
    crate::{
        sni::{MutableProperty, SniItem, SniMenuDelta},
        wayland::{
            item::{Item, Items},
            scale::{Logical, Scale},
            seat::Seat,
            sni_proxy::{event_stream, EventSink},
            tray::{item::menu::MenuId, PopupId, TrayItemId, TraySurfaceId, Trays},
        },
    },
    ahash::AHashMap,
    std::{
        convert::Infallible,
        future::poll_fn,
        io::{self, ErrorKind},
        os::fd::AsFd,
        sync::Arc,
        task::Poll,
    },
    thiserror::Error,
    tokio::io::unix::AsyncFd,
    tray::ext_tray_v1::client::{
        ext_tray_item_v1::{self, ExtTrayItemV1},
        ext_tray_v1::ExtTrayV1,
    },
    wayland_backend::protocol::WEnum,
    wayland_client::{
        delegate_noop,
        protocol::{
            wl_buffer,
            wl_callback::{self, WlCallback},
            wl_compositor,
            wl_pointer::{self, ButtonState, WlPointer},
            wl_registry,
            wl_seat::{self, WlSeat},
            wl_shm::WlShm,
            wl_shm_pool::WlShmPool,
            wl_surface,
        },
        ConnectError, Connection, Dispatch, DispatchError, QueueHandle,
    },
    wayland_protocols::{
        wp::{
            cursor_shape::v1::client::{
                wp_cursor_shape_device_v1::WpCursorShapeDeviceV1,
                wp_cursor_shape_manager_v1::WpCursorShapeManagerV1,
            },
            fractional_scale::v1::client::{
                wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
                wp_fractional_scale_v1::{self, WpFractionalScaleV1},
            },
            single_pixel_buffer::v1::client::wp_single_pixel_buffer_manager_v1::WpSinglePixelBufferManagerV1,
            viewporter::client::{wp_viewport::WpViewport, wp_viewporter},
        },
        xdg::shell::client::{
            xdg_popup::{self, XdgPopup},
            xdg_positioner::XdgPositioner,
            xdg_surface::{self, XdgSurface},
            xdg_wm_base::XdgWmBase,
        },
    },
    wl_buffer::WlBuffer,
    wl_compositor::WlCompositor,
    wl_surface::WlSurface,
    wp_viewporter::WpViewporter,
};

#[derive(Debug, Error)]
pub enum WaylandError {
    #[error("Could not connect to the compositor")]
    ConnectCompositor(#[from] ConnectError),
    #[error("Could not connect to dbus")]
    ConnectDbus(#[source] zbus::Error),
    #[error("Could not create a tokio AsyncFd from the compositor fd")]
    AsyncFd(#[source] io::Error),
    #[error("Could not poll the wayland socket")]
    PollWaylandSocket(#[source] io::Error),
    #[error("Could not dispatch wayland events")]
    WaylandDispatch(#[from] DispatchError),
    #[error("Could not send wayland message")]
    WaylandSend(#[source] wayland_backend::client::WaylandError),
    #[error("Could not read wayland messages")]
    WaylandRecv(#[source] wayland_backend::client::WaylandError),
}

pub async fn run() -> Result<Infallible, WaylandError> {
    let conn = Connection::connect_to_env()?;

    let mut event_queue = conn.new_event_queue::<State>();
    let qhandle = event_queue.handle();

    let display = conn.display();
    display.get_registry(&qhandle, ());

    display.sync(&qhandle, InitialRoundtrip);

    let dbus = zbus::Connection::session()
        .await
        .map_err(WaylandError::ConnectDbus)?;
    let dbus = bussy::Connection::wrap(&dbus);
    let (sink, mut stream) = event_stream();

    let mut state = State {
        singletons_opt: Default::default(),
        singletons: None,
        items: Default::default(),
        trays: Default::default(),
        seats: Default::default(),
        sink: sink.clone(),
        dbus: dbus.connection.clone(),
    };

    let afd = AsyncFd::new(conn.as_fd()).map_err(WaylandError::AsyncFd)?;
    poll_fn(|cx| loop {
        stream.poll(cx, &mut state);
        let registered_interest = match afd.poll_read_ready(cx) {
            Poll::Ready(r) => {
                r.map_err(WaylandError::PollWaylandSocket)?.clear_ready();
                false
            }
            Poll::Pending => true,
        };
        let mut read_any = true;
        if let Some(guard) = conn.prepare_read() {
            match guard.read() {
                Ok(0) => read_any = false,
                Ok(_) => {}
                Err(wayland_backend::client::WaylandError::Io(e))
                    if e.kind() == ErrorKind::WouldBlock =>
                {
                    read_any = false;
                }
                Err(e) => return Poll::Ready(Err(WaylandError::WaylandRecv(e))),
            }
        }
        let dispatched_any = event_queue.dispatch_pending(&mut state)? > 0;
        event_queue.flush().map_err(WaylandError::WaylandSend)?;
        if registered_interest && !read_any && !dispatched_any {
            return Poll::Pending;
        }
    })
    .await
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
    qh: QueueHandle<State>,
    wl_compositor: WlCompositor,
    wl_shm: WlShm,
    wp_viewporter: WpViewporter,
    wp_cursor_shape_manager_v1: WpCursorShapeManagerV1,
    xdg_wm_base: XdgWmBase,
    xdg_wm_base_version: u32,
    wp_fractional_scale_manager_v1: Option<WpFractionalScaleManagerV1>,
}

struct State {
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
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        use wl_registry::Event;
        match event {
            Event::Global {
                name,
                interface,
                version,
            } => match &interface[..] {
                "wl_compositor" => {
                    state.singletons_opt.wl_compositor =
                        Some(registry.bind::<WlCompositor, _, _>(name, 4, qh, ()));
                }
                "wl_shm" => {
                    state.singletons_opt.wl_shm =
                        Some(registry.bind::<WlShm, _, _>(name, 1, qh, ()));
                }
                "wp_viewporter" => {
                    state.singletons_opt.wp_viewporter =
                        Some(registry.bind::<WpViewporter, _, _>(name, 1, qh, ()));
                }
                "wp_fractional_scale_manager_v1" => {
                    state.singletons_opt.wp_fractional_scale_manager_v1 =
                        Some(registry.bind::<WpFractionalScaleManagerV1, _, _>(name, 1, qh, ()));
                }
                "xdg_wm_base" => {
                    let version = version.min(6);
                    state.singletons_opt.xdg_wm_base =
                        Some(registry.bind::<XdgWmBase, _, _>(name, version, qh, ()));
                    state.singletons_opt.xdg_wm_base_version = version;
                }
                "wp_cursor_shape_manager_v1" => {
                    state.singletons_opt.wp_cursor_shape_manager_v1 =
                        Some(registry.bind::<WpCursorShapeManagerV1, _, _>(name, 1, qh, ()));
                }
                "ext_tray_v1" => {
                    let tray = registry.bind::<ExtTrayV1, _, _>(name, 1, qh, ());
                    let tray = state.trays.create_tray(tray, name);
                    if let Some(s) = &state.singletons {
                        for item in state.items.items.values() {
                            tray.add_item(s, item);
                        }
                    }
                }
                "wl_seat" => {
                    let seat = registry.bind::<WlSeat, _, _>(name, version.min(8), qh, name);
                    state.seats.insert(name, Seat::new(seat, name));
                }
                _ => {}
            },
            Event::GlobalRemove { name } => {
                if let Some(mut seat) = state.seats.remove(&name) {
                    seat.handle_remove(&mut state.trays);
                }
                state.trays.handle_global_remove(name);
            }
            _ => {}
        }
    }
}

struct InitialRoundtrip;

impl Dispatch<WlCallback, InitialRoundtrip> for State {
    fn event(
        state: &mut Self,
        _proxy: &WlCallback,
        _event: wl_callback::Event,
        _data: &InitialRoundtrip,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
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
            qh: qh.clone(),
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

impl Dispatch<ExtTrayItemV1, TrayItemId> for State {
    fn event(
        state: &mut Self,
        _proxy: &ExtTrayItemV1,
        event: ext_tray_item_v1::Event,
        &id: &TrayItemId,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        let Some(tray_item) = state.trays.get_item_mut(id) else {
            return;
        };
        use ext_tray_item_v1::Event;
        match event {
            Event::ConfigureSize { width, height } => {
                tray_item.configure_size(Logical(width, height));
            }
            Event::Configure { serial } => {
                let Some(item) = state.items.items.get(&id.item) else {
                    return;
                };
                tray_item.configure(Some(serial), s(&state.singletons), item);
            }
        }
    }
}

impl Dispatch<WlSeat, u32> for State {
    fn event(
        state: &mut Self,
        _proxy: &WlSeat,
        event: wl_seat::Event,
        name: &u32,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        let Some(seat) = state.seats.get_mut(name) else {
            return;
        };
        use wl_seat::Event;
        match event {
            Event::Capabilities {
                capabilities: WEnum::Value(c),
            } => {
                seat.update_capabilities(s(&state.singletons), &mut state.trays, c);
            }
            _ => {}
        }
    }
}

impl Dispatch<WlPointer, u32> for State {
    fn event(
        state: &mut Self,
        _proxy: &WlPointer,
        event: wl_pointer::Event,
        &name: &u32,
        _conn: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(seat) = state.seats.get_mut(&name) else {
            return;
        };
        use wl_pointer::Event;
        match event {
            Event::Enter {
                surface,
                surface_x,
                surface_y,
                serial,
            } => {
                seat.handle_pointer_enter(
                    &state.items,
                    s(&state.singletons),
                    &mut state.trays,
                    surface,
                    surface_x as i32,
                    surface_y as i32,
                    serial,
                );
            }
            Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                seat.handle_pointer_motion(
                    &state.items,
                    s(&state.singletons),
                    &mut state.trays,
                    surface_x as i32,
                    surface_y as i32,
                    None,
                );
            }
            Event::Leave { .. } => {
                seat.handle_pointer_leave(&mut state.trays);
            }
            Event::Button {
                button,
                state: WEnum::Value(ButtonState::Pressed),
                serial,
                ..
            } => {
                seat.handle_button_pressed(
                    s(&state.singletons),
                    &mut state.trays,
                    &mut state.items,
                    button,
                    serial,
                );
            }
            Event::AxisDiscrete { .. } | Event::AxisValue120 { .. } => {
                let (axis, value120) = match event {
                    Event::AxisDiscrete { axis, discrete } => (axis, discrete * 120),
                    Event::AxisValue120 { axis, value120 } => (axis, value120),
                    _ => unreachable!(),
                };
                let WEnum::Value(axis) = axis else {
                    return;
                };
                seat.handle_axis_value120(&mut state.trays, axis, value120);
            }
            _ => {}
        }
    }
}

impl Dispatch<XdgSurface, PopupId> for State {
    fn event(
        state: &mut Self,
        _proxy: &XdgSurface,
        event: xdg_surface::Event,
        id: &PopupId,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        use xdg_surface::Event;
        match event {
            Event::Configure { serial } => {
                state.trays.handle_popup_configured(*id, serial);
            }
            _ => {}
        }
    }
}

impl Dispatch<XdgPopup, PopupId> for State {
    fn event(
        state: &mut Self,
        _proxy: &XdgPopup,
        event: xdg_popup::Event,
        id: &PopupId,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        use xdg_popup::Event;
        match event {
            Event::PopupDone => {
                state.trays.handle_popup_done(*id);
            }
            Event::Repositioned { token } => {
                state.trays.handle_popup_repositioned(*id, token);
            }
            _ => {}
        }
    }
}

impl Dispatch<WlBuffer, Option<TraySurfaceId>> for State {
    fn event(
        state: &mut Self,
        proxy: &WlBuffer,
        event: wl_buffer::Event,
        id: &Option<TraySurfaceId>,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        use wl_buffer::Event;
        match event {
            Event::Release => {
                if let Some(id) = id {
                    state.trays.handle_buffer_released(*id, proxy);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WpFractionalScaleV1, TrayItemId> for State {
    fn event(
        state: &mut Self,
        _proxy: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        id: &TrayItemId,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        use wp_fractional_scale_v1::Event;
        match event {
            Event::PreferredScale { scale } => {
                let scale = Scale(scale as _);
                state
                    .trays
                    .handle_scale(&state.items, s(&state.singletons), *id, scale);
            }
            _ => {}
        }
    }
}

impl Dispatch<WlSurface, TrayItemId> for State {
    fn event(
        state: &mut Self,
        _proxy: &WlSurface,
        event: wl_surface::Event,
        id: &TrayItemId,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        use wl_surface::Event;
        match event {
            Event::PreferredBufferScale { factor } => {
                let s = s(&state.singletons);
                if s.wp_fractional_scale_manager_v1.is_none() {
                    let scale = Scale(factor * 120);
                    state.trays.handle_scale(&state.items, s, *id, scale);
                }
            }
            _ => {}
        }
    }
}

delegate_noop!(State: ignore ExtTrayV1);
delegate_noop!(State: ignore WlCompositor);
delegate_noop!(State: ignore WlShm);
delegate_noop!(State: ignore WlShmPool);
delegate_noop!(State: ignore WlSurface);
delegate_noop!(State: ignore WpCursorShapeDeviceV1);
delegate_noop!(State: ignore WpCursorShapeManagerV1);
delegate_noop!(State: ignore WpFractionalScaleManagerV1);
delegate_noop!(State: ignore WpSinglePixelBufferManagerV1);
delegate_noop!(State: ignore WpViewport);
delegate_noop!(State: ignore WpViewporter);
delegate_noop!(State: ignore XdgPositioner);
delegate_noop!(State: ignore XdgWmBase);
