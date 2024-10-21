use {
    crate::wayland::{tray::TrayItemId, State},
    wayland_client::{
        protocol::{wl_seat::WlSeat, wl_surface::WlSurface},
        QueueHandle,
    },
    wayland_protocols::xdg::shell::client::xdg_popup::XdgPopup,
};

pub mod ext_tray_v1 {
    use {
        crate::wayland::{
            tray::{
                protocols::{
                    ext_tray_v1::client::{
                        ext_tray_item_v1::{ExtTrayItemV1, KeyboardFocusHint},
                        ext_tray_v1::ExtTrayV1,
                    },
                    ProtoName, WaylandTray, WaylandTrayItem,
                },
                TrayItemId,
            },
            State,
        },
        wayland_client::{
            protocol::{wl_seat::WlSeat, wl_surface::WlSurface},
            QueueHandle,
        },
        wayland_protocols::xdg::shell::client::xdg_popup::XdgPopup,
    };

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
            wayland_scanner::generate_interfaces!("ext-tray-v1.xml");
        }
        wayland_scanner::generate_client_code!("ext-tray-v1.xml");
    }

    impl WaylandTray for ExtTrayV1 {
        fn proto_name(&self) -> ProtoName {
            ProtoName::ExtTrayV1
        }

        fn get_tray_item(
            &self,
            surface: &WlSurface,
            qh: &QueueHandle<State>,
            id: TrayItemId,
        ) -> Box<dyn WaylandTrayItem> {
            Box::new(self.get_tray_item(surface, qh, id))
        }
    }

    impl WaylandTrayItem for ExtTrayItemV1 {
        fn destroy(&self) {
            self.destroy();
        }

        fn ack_configure(&self, serial: u32) {
            self.ack_configure(serial);
        }

        fn get_popup(&self, popup: &XdgPopup, seat: &WlSeat, serial: u32) {
            self.get_popup(popup, seat, serial, KeyboardFocusHint::None);
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ProtoName {
    ExtTrayV1,
}

pub trait WaylandTray {
    fn proto_name(&self) -> ProtoName;
    fn get_tray_item(
        &self,
        surface: &WlSurface,
        qh: &QueueHandle<State>,
        id: TrayItemId,
    ) -> Box<dyn WaylandTrayItem>;
}

pub trait WaylandTrayItem {
    fn destroy(&self);
    fn ack_configure(&self, serial: u32);
    fn get_popup(&self, popup: &XdgPopup, seat: &WlSeat, serial: u32);
}
