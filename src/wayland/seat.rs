use {
    crate::wayland::{
        item::Items,
        tray::{item::menu::MenuId, TraySurfaceId, Trays},
        Singletons,
    },
    std::{
        sync::atomic::{AtomicUsize, Ordering::Relaxed},
        time::Duration,
    },
    tokio::task::JoinHandle,
    wayland_client::protocol::{
        wl_pointer::{Axis, WlPointer},
        wl_seat::{Capability, WlSeat},
        wl_surface::WlSurface,
    },
    wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
        Shape, WpCursorShapeDeviceV1,
    },
};

pub struct Seat {
    name: u32,
    seat: WlSeat,
    pointer: Option<Pointer>,
    focus: Option<TraySurfaceId>,
    x: i32,
    y: i32,
    scroll: [i32; 2],
    timeout: Option<Timeout>,
}

struct Pointer {
    pointer: WlPointer,
    shape: WpCursorShapeDeviceV1,
}

impl Drop for Pointer {
    fn drop(&mut self) {
        self.shape.destroy();
        self.pointer.release();
    }
}

struct Timeout {
    id: usize,
    target: MotionTimeoutTarget,
    future: JoinHandle<()>,
}

impl Drop for Timeout {
    fn drop(&mut self) {
        self.future.abort();
    }
}

pub enum MotionResult {
    None,
    ContinueTimeout {
        timeout: Duration,
        target: MotionTimeoutTarget,
    },
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MotionTimeoutTarget {
    pub menu_id: Option<MenuId>,
}

impl Seat {
    pub fn new(seat: WlSeat, name: u32) -> Self {
        Self {
            name,
            seat,
            pointer: None,
            focus: None,
            x: 0,
            y: 0,
            scroll: [0; 2],
            timeout: None,
        }
    }

    pub fn name(&self) -> u32 {
        self.name
    }

    pub fn wl_seat(&self) -> &WlSeat {
        &self.seat
    }

    pub fn update_capabilities(
        &mut self,
        s: &Singletons,
        trays: &mut Trays,
        capabilities: Capability,
    ) {
        let want_pointer = capabilities.contains(Capability::Pointer);
        if want_pointer {
            if self.pointer.is_none() {
                let pointer = self.seat.get_pointer(&s.qh, self.name);
                let shape = s
                    .wp_cursor_shape_manager_v1
                    .get_pointer(&pointer, &s.qh, ());
                self.pointer = Some(Pointer { pointer, shape });
            }
        } else {
            if self.pointer.take().is_some() {
                self.handle_pointer_leave(trays);
            }
        }
    }

    pub fn handle_pointer_leave(&mut self, trays: &mut Trays) {
        self.timeout = None;
        if let Some(focus) = self.focus.take() {
            trays.handle_leave(self, focus);
        }
    }

    #[expect(clippy::too_many_arguments)]
    pub fn handle_pointer_enter(
        &mut self,
        items: &Items,
        s: &Singletons,
        trays: &mut Trays,
        surface: WlSurface,
        x: i32,
        y: i32,
        serial: u32,
    ) {
        self.handle_pointer_leave(trays);
        if let Some(pointer) = &self.pointer {
            pointer.shape.set_shape(serial, Shape::Default);
        }
        let Some(surface) = trays.find_surface(&surface) else {
            return;
        };
        self.focus = Some(surface);
        self.handle_pointer_motion(items, s, trays, x, y, Some(serial));
    }

    pub fn handle_pointer_motion(
        &mut self,
        items: &Items,
        s: &Singletons,
        trays: &mut Trays,
        x: i32,
        y: i32,
        serial: Option<u32>,
    ) {
        self.x = x;
        self.y = y;
        let Some(focus) = self.focus else {
            return;
        };
        let res = trays.handle_motion(self, serial, items, s, focus, x, y);
        let MotionResult::ContinueTimeout { timeout, target } = res else {
            self.timeout = None;
            return;
        };
        if let Some(t) = &self.timeout {
            if t.target == target {
                return;
            }
            self.timeout = None;
        }
        let seat_name = self.name;
        let sink = s.sink.clone();
        static TIMEOUT_IDS: AtomicUsize = AtomicUsize::new(0);
        let id = TIMEOUT_IDS.fetch_add(1, Relaxed);
        let future = tokio::task::spawn(async move {
            tokio::time::sleep(timeout).await;
            sink.send(move |state| {
                state.handle_seat_timeout(seat_name, id);
            });
        });
        self.timeout = Some(Timeout { id, target, future });
    }

    pub fn handle_timeout(&mut self, items: &Items, s: &Singletons, trays: &mut Trays, id: usize) {
        if let Some(timeout) = &self.timeout {
            if timeout.id != id {
                return;
            }
        };
        let Some(timeout) = self.timeout.take() else {
            return;
        };
        let Some(focus) = self.focus else {
            return;
        };
        trays.handle_timeout(self, items, s, focus, timeout.target.menu_id);
    }

    pub fn handle_button_pressed(
        &mut self,
        s: &Singletons,
        trays: &mut Trays,
        items: &mut Items,
        button: u32,
        serial: u32,
    ) {
        let Some(id) = self.focus else {
            return;
        };
        let Some(item) = items.items.get(&id.item.item) else {
            return;
        };
        trays.handle_button(self, serial, id, s, item, button);
    }

    pub fn handle_axis_value120(&mut self, trays: &mut Trays, axis: Axis, value120: i32) {
        let accu = &mut self.scroll[axis as usize];
        *accu += value120;
        let steps = *accu / 120;
        *accu -= steps * 120;
        let Some(focus) = self.focus else {
            return;
        };
        trays.handle_scroll(focus, axis, steps);
    }

    pub fn handle_remove(&mut self, trays: &mut Trays) {
        if let Some(focus) = self.focus {
            trays.handle_leave(self, focus);
        }
    }
}
