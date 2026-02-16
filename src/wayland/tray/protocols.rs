use crate::wayland::{
    protocols::{
        wayland::{wl_seat::WlSeat, wl_surface::WlSurface},
        xdg_shell::xdg_popup::XdgPopup,
    },
    tray::TrayItemId,
};

pub mod ext_tray_v1 {
    use {
        crate::wayland::{
            ExtTrayItemV1Handler,
            protocols::{
                ext_tray_v1::{
                    ext_tray_item_v1::{ExtTrayItemV1, ExtTrayItemV1KeyboardFocusHint},
                    ext_tray_v1::ExtTrayV1,
                },
                wayland::{wl_seat::WlSeat, wl_surface::WlSurface},
                xdg_shell::xdg_popup::XdgPopup,
            },
            tray::{
                TrayItemId,
                protocols::{ProtoName, WaylandTray, WaylandTrayItem},
            },
        },
        std::ops::Deref,
        wl_client::proxy,
    };

    impl WaylandTray for ExtTrayV1 {
        fn proto_name(&self) -> ProtoName {
            ProtoName::ExtTrayV1
        }

        fn get_tray_item(&self, surface: &WlSurface, id: TrayItemId) -> Box<dyn WaylandTrayItem> {
            let item = self.get_tray_item(surface);
            proxy::set_event_handler(&item, ExtTrayItemV1Handler { id });
            Box::new(item)
        }
    }

    impl WaylandTrayItem for ExtTrayItemV1 {
        fn destroy(&self) {
            self.destroy();
        }

        fn ack_configure(&self, serial: u32) {
            self.deref().ack_configure(serial);
        }

        fn get_popup(&self, popup: &XdgPopup, seat: &WlSeat, serial: u32) {
            self.deref()
                .get_popup(popup, seat, serial, ExtTrayItemV1KeyboardFocusHint::NONE);
        }
    }
}

pub mod jay_tray_v1 {
    use {
        crate::wayland::{
            JayTrayItemV1Handler,
            protocols::{
                jay_tray_v1::{
                    jay_tray_item_v1::{JayTrayItemV1, JayTrayItemV1KeyboardFocusHint},
                    jay_tray_v1::JayTrayV1,
                },
                wayland::{wl_seat::WlSeat, wl_surface::WlSurface},
                xdg_shell::xdg_popup::XdgPopup,
            },
            tray::{
                TrayItemId,
                protocols::{ProtoName, WaylandTray, WaylandTrayItem},
            },
        },
        std::ops::Deref,
        wl_client::proxy,
    };

    impl WaylandTray for JayTrayV1 {
        fn proto_name(&self) -> ProtoName {
            ProtoName::JayTrayV1
        }

        fn get_tray_item(&self, surface: &WlSurface, id: TrayItemId) -> Box<dyn WaylandTrayItem> {
            let item = self.get_tray_item(surface);
            proxy::set_event_handler(&item, JayTrayItemV1Handler { id });
            Box::new(item)
        }
    }

    impl WaylandTrayItem for JayTrayItemV1 {
        fn destroy(&self) {
            self.destroy();
        }

        fn ack_configure(&self, serial: u32) {
            self.deref().ack_configure(serial);
        }

        fn get_popup(&self, popup: &XdgPopup, seat: &WlSeat, serial: u32) {
            self.deref()
                .get_popup(popup, seat, serial, JayTrayItemV1KeyboardFocusHint::NONE);
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ProtoName {
    ExtTrayV1,
    JayTrayV1,
}

pub trait WaylandTray {
    fn proto_name(&self) -> ProtoName;
    fn get_tray_item(&self, surface: &WlSurface, id: TrayItemId) -> Box<dyn WaylandTrayItem>;
}

pub trait WaylandTrayItem {
    fn destroy(&self);
    fn ack_configure(&self, serial: u32);
    fn get_popup(&self, popup: &XdgPopup, seat: &WlSeat, serial: u32);
}
