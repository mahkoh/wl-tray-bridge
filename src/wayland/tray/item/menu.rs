use {
    crate::{
        settings::{self},
        sni::{IconFrame, IconFrames, SniItem, SniMenuDelta, SniMenuToggleType},
        wayland::{
            item::Items,
            scale::{Logical, Physical, Scale},
            seat::{MotionResult, Seat},
            tray::{
                item::{
                    icon::{render_png, CairoIcon, IconTemplate},
                    TrayItem,
                },
                PopupIdType, TraySurfaceId,
            },
            utils::create_shm_buf,
            PopupId, Singletons, TrayItemId,
        },
    },
    ahash::{AHashMap, AHashSet},
    bussy::Call,
    error_reporter::Report,
    isnt::std_1::primitive::IsntStrExt,
    memfile::MemFile,
    pangocairo::{
        cairo::{self, Format, LineCap},
        functions::show_layout,
        pango::{self},
        FontMap,
    },
    std::{
        f64::consts::PI,
        io::{self, Seek, SeekFrom, Write},
        mem,
        sync::Arc,
    },
    thiserror::Error,
    wayland_client::protocol::{wl_buffer::WlBuffer, wl_surface::WlSurface},
    wayland_protocols::{
        wp::viewporter::client::wp_viewport::WpViewport,
        xdg::shell::client::{
            xdg_popup::XdgPopup,
            xdg_positioner::{Anchor, ConstraintAdjustment, Gravity, XdgPositioner},
            xdg_surface::XdgSurface,
        },
    },
};

#[derive(Debug, Error)]
pub enum MenuError {
    #[error(transparent)]
    Cairo(#[from] cairo::Error),
    #[error(transparent)]
    Borrow(#[from] cairo::BorrowError),
    #[error("Could not create memfd")]
    CreateMemfd(#[source] io::Error),
    #[error("Could not update memfd")]
    UpdateMemfd(#[source] io::Error),
}

pub type MenuId = i32;

#[derive(Default)]
pub struct Menu {
    items: AHashMap<MenuId, MenuItem>,
}

pub struct MenuInstance {
    sni: Arc<SniItem>,
    tray_item: TrayItemId,
    scale: Scale,
    icon_cache: AHashMap<MenuId, CairoIcon>,
    open_call: Option<Call>,
    open: OpenMenu,
}

#[derive(Default)]
pub struct SubMenu {
    id: MenuId,
    items: Vec<MenuId>,
}

struct MenuItem {
    id: MenuId,
    separator: bool,
    label: Option<Arc<String>>,
    enabled: bool,
    visible: bool,
    icon_template: IconTemplate,
    toggle_type: Option<SniMenuToggleType>,
    toggle_active: bool,
    submenu: Option<SubMenu>,
}

pub struct OpenMenu {
    id: MenuId,
    tray_item: TrayItemId,
    log_size: Logical,
    phy_size: Physical,
    front_buffer: MenuBuffer,
    back_buffer: MenuBuffer,
    surface: WlSurface,
    viewport: WpViewport,
    xdg_surface: XdgSurface,
    xdg_popup: XdgPopup,
    child: Option<Box<OpenMenu>>,
    rows: Vec<OpenMenuRow>,
    next_reposition: u32,
    awaiting_reposition: Option<u32>,
    is_configured: bool,
    needs_swap: bool,
    needs_render: bool,
    seat_position: AHashMap<u32, i32>,
    seat_hover: AHashMap<u32, MenuId>,
    positioner: XdgPositioner,
    can_reposition: bool,
}

#[derive(Debug)]
struct RenderedMenu {
    buffer: Vec<u8>,
    log_space_top: i32,
    log_size: Logical,
    phy_size: Physical,
    rows: Vec<OpenMenuRow>,
}

#[derive(Copy, Clone, Debug)]
struct OpenMenuRow {
    y1: i32,
    y2: i32,
    menu_id: MenuId,
}

struct MenuBuffer {
    buffer: WlBuffer,
    memfile: MemFile,
    free: bool,
}

impl Drop for MenuBuffer {
    fn drop(&mut self) {
        self.buffer.destroy();
    }
}

impl Drop for OpenMenu {
    fn drop(&mut self) {
        self.child = None;
        self.xdg_popup.destroy();
        self.xdg_surface.destroy();
        self.viewport.destroy();
        self.surface.destroy();
        self.positioner.destroy();
    }
}

impl Menu {
    pub fn apply_delta(&mut self, delta: &SniMenuDelta) {
        let mut remove = vec![];
        self.apply_delta2(&mut AHashSet::new(), &mut remove, delta);
        for item in remove {
            self.items.remove(&item);
        }
    }

    fn apply_delta2(
        &mut self,
        seen: &mut AHashSet<MenuId>,
        remove: &mut Vec<MenuId>,
        delta: &SniMenuDelta,
    ) {
        if !seen.insert(delta.menu_id) {
            return;
        }
        let item = self.items.entry(delta.menu_id).or_insert_with(|| MenuItem {
            id: delta.menu_id,
            separator: false,
            label: None,
            enabled: false,
            visible: false,
            icon_template: Default::default(),
            toggle_type: None,
            toggle_active: false,
            submenu: None,
        });
        if let Some(p) = &delta.properties {
            if let Some(v) = p.separator {
                item.separator = v;
            }
            if let Some(v) = &p.label {
                item.label = v.is_not_empty().then(|| v.clone());
            }
            if let Some(v) = p.enabled {
                item.enabled = v;
            }
            if let Some(v) = p.visible {
                item.visible = v;
            }
            if let Some(v) = p.toggle_type {
                item.toggle_type = v;
            }
            if let Some(v) = p.toggle_state {
                item.toggle_active = v;
            }
            if let Some(v) = &p.icon_name {
                item.icon_template
                    .update_name(v.is_not_empty().then_some(v), None);
            }
            if let Some(v) = &p.icon_png {
                if v.is_empty() {
                    item.icon_template.update_frames(None);
                } else {
                    match render_png(v) {
                        Ok((v, size)) => {
                            let frame = IconFrame { bytes: v, size };
                            let frames = IconFrames {
                                frames: Arc::new(vec![frame]),
                            };
                            item.icon_template.update_frames(Some(&frames));
                        }
                        Err(e) => {
                            log::error!("Could not decode menu png icon: {}", Report::new(e));
                            item.icon_template.update_frames(None);
                        }
                    }
                }
            }
        }
        if let Some(children) = &delta.children {
            if let Some(sub) = &item.submenu {
                for item in &sub.items {
                    if !children.contains_key(item) {
                        remove.push(*item);
                    }
                }
            }
            if children.is_empty() {
                item.submenu = None;
            } else {
                let submenu = item.submenu.get_or_insert_with(|| SubMenu {
                    id: item.id,
                    items: vec![],
                });
                submenu.items.clear();
                submenu.items.extend(children.keys().copied());
                for d in children.values().flatten() {
                    self.apply_delta2(seen, remove, d);
                }
            }
        }
    }
}

impl MenuInstance {
    pub fn new(
        seat: &Seat,
        serial: u32,
        tray_item: &TrayItem,
        root: &Menu,
        s: &Singletons,
    ) -> Option<Self> {
        Self::try_new(seat, serial, tray_item, root, s).unwrap_or_else(|e| {
            log::error!("Could not create menu: {}", Report::new(e));
            None
        })
    }

    fn try_new(
        seat: &Seat,
        serial: u32,
        tray_item: &TrayItem,
        root: &Menu,
        s: &Singletons,
    ) -> Result<Option<Self>, MenuError> {
        let Some(menu) = root.items.get(&0) else {
            return Ok(None);
        };
        let Some(submenu) = menu.submenu.as_ref() else {
            return Ok(None);
        };
        let mut icon_cache = AHashMap::new();
        let seat_hover = AHashMap::new();
        let rendered = render(&mut icon_cache, &seat_hover, tray_item.scale, root, submenu)?;
        let Some(rendered) = rendered else {
            return Ok(None);
        };
        let positioner = s.xdg_wm_base.create_positioner(&s.qh, ());
        positioner.set_anchor_rect(0, 0, tray_item.size.0, tray_item.size.1);
        positioner.set_anchor(tray_item.preferred_anchor);
        positioner.set_gravity(tray_item.preferred_gravity);
        positioner.set_size(rendered.log_size.0, rendered.log_size.1);
        positioner
            .set_constraint_adjustment(ConstraintAdjustment::SlideX | ConstraintAdjustment::FlipY);
        let open = open(tray_item.id, submenu, None, positioner, s, rendered)?;
        tray_item
            .item
            .get_popup(&open.xdg_popup, seat.wl_seat(), serial);
        open.surface.commit();
        Ok(Some(Self {
            sni: tray_item.sni.clone(),
            tray_item: tray_item.id,
            scale: tray_item.scale,
            icon_cache,
            open_call: None,
            open,
        }))
    }

    pub fn hover_child(&mut self, seat_name: u32, root: &Menu, s: &Singletons, id: MenuId) {
        let Some(menu) = root.items.get(&id) else {
            return;
        };
        if !menu.enabled || menu.separator {
            return;
        }
        self.sni.menu_hovered(id);
        if menu.submenu.is_none() {
            return;
        }
        let tray_item = self.tray_item;
        let sink = s.sink.clone();
        let call = self.sni.open_menu(
            id,
            Box::new(move || {
                sink.send(move |state| {
                    state.open_menu(seat_name, tray_item, id);
                });
            }),
        );
        if call.is_none() {
            self.open_child(root, s, id);
        }
        self.open_call = call;
    }

    pub fn open_child(&mut self, root: &Menu, s: &Singletons, id: MenuId) {
        if let Err(e) = self.try_open_child(root, s, id) {
            log::error!("Could not open child menu: {}", Report::new(e));
        }
    }

    fn try_open_child(&mut self, root: &Menu, s: &Singletons, id: MenuId) -> Result<(), MenuError> {
        let Some((y1, y2, parent)) = self.open.find_child_position(id) else {
            return Ok(());
        };
        let Some(menu) = root.items.get(&id) else {
            return Ok(());
        };
        let Some(submenu) = menu.submenu.as_ref() else {
            return Ok(());
        };
        let rendered = render(
            &mut self.icon_cache,
            &AHashMap::new(),
            self.scale,
            root,
            submenu,
        )?;
        let Some(rendered) = rendered else {
            return Ok(());
        };
        let y1 = y1 - rendered.log_space_top;
        let positioner = s.xdg_wm_base.create_positioner(&s.qh, ());
        positioner.set_size(rendered.log_size.0, rendered.log_size.1);
        positioner.set_anchor_rect(0, y1, parent.log_size.0, y2 - y1);
        match settings::get().menu.rtl {
            true => {
                positioner.set_anchor(Anchor::TopLeft);
                positioner.set_gravity(Gravity::BottomLeft);
            }
            false => {
                positioner.set_anchor(Anchor::TopRight);
                positioner.set_gravity(Gravity::BottomRight);
            }
        }
        positioner
            .set_constraint_adjustment(ConstraintAdjustment::FlipX | ConstraintAdjustment::SlideY);
        let open = open(
            self.tray_item,
            submenu,
            Some(&parent.xdg_surface),
            positioner,
            s,
            rendered,
        )?;
        open.surface.commit();
        parent.child = Some(Box::new(open));
        Ok(())
    }

    pub fn apply_delta(
        &mut self,
        root: &Menu,
        delta: &SniMenuDelta,
        singletons: &Singletons,
    ) -> bool {
        let Some(menu) = root.items.get(&0) else {
            return false;
        };
        let Some(menu) = &menu.submenu else {
            return false;
        };
        self.open.apply_delta(delta);
        let rerendered =
            self.open
                .maybe_rerender(&mut self.icon_cache, self.scale, root, menu, singletons);
        if rerendered {
            let seats: Vec<_> = self.open.seat_position.keys().copied().collect();
            for seat in seats {
                let new = self.open.handle_seat_position(
                    root,
                    singletons,
                    seat,
                    &mut self.icon_cache,
                    self.scale,
                );
                if let Some(new) = new {
                    self.hover_child(seat, root, singletons, new);
                }
            }
        }
        rerendered
    }

    pub fn repositioned(&mut self, id: MenuId, token: u32) {
        self.open.repositioned(id, token);
    }

    pub fn handle_buffer_released(&mut self, menu: MenuId, buffer: &WlBuffer) {
        self.open.handle_buffer_released(menu, buffer);
    }

    pub fn configured(&mut self, id: MenuId, serial: u32) {
        self.open.configured(id, serial);
    }

    pub fn popup_done(&mut self, id: MenuId) -> bool {
        self.open.popup_done(id)
    }

    pub fn find_surface(&self, surface: &WlSurface) -> Option<TraySurfaceId> {
        Some(TraySurfaceId {
            item: self.tray_item,
            menu: Some(self.open.find_surface(surface)?),
        })
    }

    pub fn handle_button(&mut self, seat: &Seat, root: &Menu, menu: MenuId) -> bool {
        let Some(menu) = self.open.find_menu_mut(menu) else {
            return false;
        };
        let Some(target) = menu.seat_hover.get(&seat.name()) else {
            return false;
        };
        let Some(target) = root.items.get(target) else {
            return false;
        };
        if target.submenu.is_some() {
            return false;
        };
        self.sni.menu_clicked(target.id);
        true
    }

    pub fn handle_motion(
        &mut self,
        seat: &Seat,
        root: &Menu,
        s: &Singletons,
        menu_id: MenuId,
        _x: i32,
        y: i32,
    ) -> MotionResult {
        let Some(open) = self.open.find_menu_mut(menu_id) else {
            return MotionResult::None;
        };
        open.seat_position.insert(seat.name(), y);
        if let Some(new) =
            open.handle_seat_position(root, s, seat.name(), &mut self.icon_cache, self.scale)
        {
            self.hover_child(seat.name(), root, s, new);
        }
        MotionResult::None
    }

    pub fn handle_leave(&mut self, seat: &Seat, menu_id: MenuId) {
        let Some(open) = self.open.find_menu_mut(menu_id) else {
            return;
        };
        open.seat_position.remove(&seat.name());
    }

    pub fn handle_timeout(
        &mut self,
        _seat: &Seat,
        _items: &Items,
        _s: &Singletons,
        _menu: MenuId,
        _sub_menu_id: Option<MenuId>,
    ) {
        // nothing
    }
}

impl OpenMenu {
    pub fn find_surface(&self, surface: &WlSurface) -> Option<MenuId> {
        if &self.surface == surface {
            return Some(self.id);
        }
        if let Some(menu) = &self.child {
            return menu.find_surface(surface);
        }
        None
    }

    pub fn find_menu_mut(&mut self, id: MenuId) -> Option<&mut Self> {
        if self.id == id {
            return Some(self);
        }
        if let Some(menu) = &mut self.child {
            return menu.find_menu_mut(id);
        }
        None
    }

    fn repositioned(&mut self, id: MenuId, token: u32) {
        if self.id == id {
            if self.awaiting_reposition == Some(token) {
                self.awaiting_reposition = None;
            }
        } else {
            if let Some(child) = &mut self.child {
                child.repositioned(id, token);
            }
        }
    }

    pub fn handle_buffer_released(&mut self, id: MenuId, buffer: &WlBuffer) {
        if self.id == id {
            for menu_buffer in [&mut self.back_buffer, &mut self.front_buffer] {
                if menu_buffer.buffer == *buffer {
                    menu_buffer.free = true;
                }
            }
        } else {
            if let Some(child) = &mut self.child {
                child.handle_buffer_released(id, buffer);
            }
        }
    }

    fn configured(&mut self, id: MenuId, serial: u32) {
        if self.id == id {
            self.xdg_surface.ack_configure(serial);
            if self.awaiting_reposition.is_none() {
                self.is_configured = true;
                if mem::take(&mut self.needs_swap) {
                    self.swap();
                }
            }
        } else {
            if let Some(child) = &mut self.child {
                child.configured(id, serial);
            }
        }
    }

    pub fn popup_done(&mut self, id: MenuId) -> bool {
        if self.id == id {
            return false;
        }
        if let Some(open) = &mut self.child {
            if !open.popup_done(id) {
                self.child = None;
            }
        }
        true
    }

    fn find_child_position(&mut self, id: MenuId) -> Option<(i32, i32, &mut OpenMenu)> {
        let row = self.rows.iter().find(|r| r.menu_id == id);
        if let Some(row) = row {
            return Some((row.y1, row.y2, self));
        }
        if let Some(child) = &mut self.child {
            return child.find_child_position(id);
        }
        None
    }

    fn find_child_at(&self, y: i32) -> Option<MenuId> {
        for row in &self.rows {
            if row.y1 <= y && y < row.y2 {
                return Some(row.menu_id);
            }
        }
        None
    }

    fn apply_delta(&mut self, delta: &SniMenuDelta) {
        if self.id != delta.menu_id {
            return;
        }
        let mut needs_render = false;
        if let Some(c) = &delta.children {
            if c.len() != self.rows.len() {
                needs_render = true;
            } else {
                for (idx, (&id, delta)) in c.iter().enumerate() {
                    if self.rows[idx].menu_id != id {
                        needs_render = true;
                    } else if let Some(d) = delta {
                        if let Some(p) = &d.properties {
                            needs_render |= p.separator.is_some();
                            needs_render |= p.label.is_some();
                            needs_render |= p.enabled.is_some();
                            needs_render |= p.visible.is_some();
                            needs_render |= p.toggle_type.is_some();
                            needs_render |= p.toggle_state.is_some();
                            needs_render |= p.icon_name.is_some();
                            needs_render |= p.icon_png.is_some();
                        }
                    }
                }
                if let Some(child) = &mut self.child {
                    if let Some(d) = c.get(&child.id) {
                        if let Some(d) = d {
                            child.apply_delta(d);
                        }
                    } else {
                        self.child = None;
                    }
                }
            }
        }
        self.needs_render |= needs_render;
    }

    fn maybe_rerender(
        &mut self,
        icon_cache: &mut AHashMap<MenuId, CairoIcon>,
        scale: Scale,
        root: &Menu,
        menu: &SubMenu,
        s: &Singletons,
    ) -> bool {
        self.try_maybe_rerender(icon_cache, scale, root, menu, s)
            .unwrap_or_else(|e| {
                log::error!("Could not re-render menu: {}", Report::new(e));
                false
            })
    }

    fn try_maybe_rerender(
        &mut self,
        icon_cache: &mut AHashMap<MenuId, CairoIcon>,
        scale: Scale,
        root: &Menu,
        menu: &SubMenu,
        s: &Singletons,
    ) -> Result<bool, MenuError> {
        if self.needs_render {
            self.needs_render = false;
            let rendered = render(icon_cache, &self.seat_hover, scale, root, menu)?;
            let Some(rendered) = rendered else {
                return Ok(false);
            };
            let create_buffer = || {
                create_buffer(self.tray_item, menu, s, &rendered).map_err(MenuError::CreateMemfd)
            };
            if rendered.phy_size != self.phy_size {
                self.front_buffer = create_buffer()?;
                self.back_buffer = create_buffer()?;
            } else {
                if !self.back_buffer.free {
                    self.back_buffer = create_buffer()?;
                }
                self.back_buffer
                    .memfile
                    .seek(SeekFrom::Start(0))
                    .map_err(MenuError::UpdateMemfd)?;
                self.back_buffer
                    .memfile
                    .write_all(&rendered.buffer)
                    .map_err(MenuError::UpdateMemfd)?;
            }
            if rendered.log_size != self.log_size && self.can_reposition {
                self.next_reposition = self.next_reposition.wrapping_add(1);
                self.awaiting_reposition = Some(self.next_reposition);
                self.is_configured = false;
                self.positioner
                    .set_size(rendered.log_size.0, rendered.log_size.1);
                self.xdg_popup
                    .reposition(&self.positioner, self.next_reposition);
            }
            self.log_size = rendered.log_size;
            self.phy_size = rendered.phy_size;
            self.rows = rendered.rows;
            if self.is_configured {
                self.swap();
            } else {
                self.needs_swap = true;
            }
        }
        if let Some(child) = &mut self.child {
            let Some(menu) = root.items.get(&child.id) else {
                self.child = None;
                return Ok(true);
            };
            let Some(menu) = &menu.submenu else {
                self.child = None;
                return Ok(true);
            };
            if !child.maybe_rerender(icon_cache, scale, root, menu, s) {
                self.child = None;
            }
        }
        Ok(true)
    }

    fn swap(&mut self) {
        mem::swap(&mut self.front_buffer, &mut self.back_buffer);
        self.front_buffer.free = false;
        self.viewport
            .set_destination(self.log_size.0, self.log_size.1);
        self.surface.attach(Some(&self.front_buffer.buffer), 0, 0);
        self.surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
        self.surface.commit();
    }

    fn handle_seat_position(
        &mut self,
        root: &Menu,
        s: &Singletons,
        seat_name: u32,
        icon_cache: &mut AHashMap<MenuId, CairoIcon>,
        scale: Scale,
    ) -> Option<MenuId> {
        let new = self
            .seat_position
            .get(&seat_name)
            .and_then(|y| self.find_child_at(*y));
        let old = new.and_then(|n| self.seat_hover.insert(seat_name, n));
        if old == new {
            return None;
        }
        self.child = None;
        self.needs_render = true;
        if let Some(menu) = root.items.get(&self.id) {
            if let Some(sub) = &menu.submenu {
                self.maybe_rerender(icon_cache, scale, root, sub, s);
            }
        }
        new
    }
}

fn create_buffer(
    tray_item: TrayItemId,
    menu: &SubMenu,
    s: &Singletons,
    rendered: &RenderedMenu,
) -> Result<MenuBuffer, io::Error> {
    let id = TraySurfaceId {
        item: tray_item,
        menu: Some(menu.id),
    };
    let (buffer, memfile) =
        create_shm_buf(s, &rendered.buffer, rendered.phy_size.size(), Some(id))?;
    Ok(MenuBuffer {
        buffer,
        memfile,
        free: true,
    })
}

fn open(
    tray_item: TrayItemId,
    menu: &SubMenu,
    parent: Option<&XdgSurface>,
    positioner: XdgPositioner,
    s: &Singletons,
    rendered: RenderedMenu,
) -> Result<OpenMenu, MenuError> {
    let id = PopupId {
        tray_item,
        ty: PopupIdType::MenuId(menu.id),
    };
    let surface = s.wl_compositor.create_surface(&s.qh, ());
    let viewport = s.wp_viewporter.get_viewport(&surface, &s.qh, ());
    let xdg_surface = s.xdg_wm_base.get_xdg_surface(&surface, &s.qh, id);
    let xdg_popup = xdg_surface.get_popup(parent, &positioner, &s.qh, id);
    let create_buffer =
        || create_buffer(tray_item, menu, s, &rendered).map_err(MenuError::CreateMemfd);
    Ok(OpenMenu {
        id: menu.id,
        tray_item,
        log_size: rendered.log_size,
        phy_size: rendered.phy_size,
        front_buffer: create_buffer()?,
        back_buffer: create_buffer()?,
        surface,
        viewport,
        xdg_surface,
        xdg_popup,
        child: None,
        rows: rendered.rows,
        next_reposition: 0,
        awaiting_reposition: None,
        is_configured: false,
        needs_swap: true,
        needs_render: false,
        seat_position: Default::default(),
        seat_hover: Default::default(),
        positioner,
        can_reposition: s.xdg_wm_base_version >= 3,
    })
}

fn render(
    icon_cache: &mut AHashMap<MenuId, CairoIcon>,
    hovered: &AHashMap<u32, MenuId>,
    scale: Scale,
    root: &Menu,
    menu: &SubMenu,
) -> Result<Option<RenderedMenu>, MenuError> {
    let settings = settings::get();
    let wlscale = scale.to_f64();
    let scalef = wlscale * settings.scale;
    let pango_scale = pango::SCALE as f64;

    let mut has_icons = false;
    let mut has_submenus = false;
    let mut max_label_width = 0.0f64;
    let mut max_label_height = 0.0f64;
    let mut num_labels = 0;
    let mut num_separators = 0;

    let ctx = pango::Context::new();
    ctx.set_font_map(Some(&FontMap::default()));
    let mut font = settings.menu.font.clone();
    font.set_size((font.size() as f64 * scalef).round() as _);
    let font_size = font.size() as f64 / pango_scale;
    ctx.set_font_description(Some(&font));
    let layout = pango::Layout::new(&ctx);

    let line_width = scalef.round();
    let border_width = (settings.menu.border_width * scalef).round();
    let padding = (settings.menu.padding * scalef).round();
    let box_width = (font_size / 2.0).ceil() * 2.0;
    let sub_width = box_width * 1.5 / 3.0;

    let mut items = vec![];
    for item in &menu.items {
        let Some(item) = root.items.get(item) else {
            continue;
        };
        if !item.visible {
            continue;
        }
        items.push(item);
        if item.separator {
            num_separators += 1;
            continue;
        }
        num_labels += 1;
        has_icons |= item.icon_template.is_some();
        has_submenus |= item.submenu.is_some();
        let label = match &item.label {
            None => "",
            Some(l) => l,
        };
        layout.set_text(label);
        let (w, h) = layout.size();
        let mut w = w as f64 / pango_scale;
        let h = h as f64 / pango_scale;
        if item.toggle_type.is_some() {
            w += box_width + 2.0 * padding;
        }
        max_label_width = max_label_width.max(w);
        max_label_height = max_label_height.max(h);
    }

    if num_labels == 0 {
        return Ok(None);
    }

    let mut phy_width = max_label_width;
    phy_width += 2.0 * padding;
    phy_width += 2.0 * border_width;
    if has_icons {
        phy_width += box_width + 2.0 * padding;
    }
    if has_submenus {
        phy_width += sub_width + 2.0 * padding;
    }
    let mut phy_height = padding;
    phy_height += 2.0 * border_width;
    phy_height += (max_label_height + padding) * num_labels as f64;
    phy_height += (line_width + padding) * num_separators as f64;

    let log = Logical(
        (phy_width / wlscale).ceil() as i32,
        (phy_height / wlscale).ceil() as i32,
    );
    let phy = log.to_physical(scale);

    let mut surface = cairo::ImageSurface::create(Format::ARgb32, phy.0, phy.1)?;
    let cairo = cairo::Context::new(&surface)?;

    let mut rows = Vec::<(f64, f64, MenuId)>::new();

    // background
    settings.menu.background_color.set(&cairo);
    cairo.paint()?;

    // items
    let mut y = border_width + padding;
    for item in items {
        cairo.move_to(border_width + padding, y);
        if item.separator {
            cairo.move_to(border_width + line_width / 2.0, y + line_width / 2.0);
            cairo.line_to(
                phy.0 as f64 - border_width - line_width / 2.0,
                y + line_width / 2.0,
            );
            cairo.set_line_width(line_width);
            cairo.set_line_cap(LineCap::Square);
            settings.menu.border_color.set(&cairo);
            cairo.stroke()?;
            y += line_width;
        } else {
            let mut x = border_width + padding;
            let mut color = &settings.menu.color;
            if !item.enabled {
                color = &settings.menu.disabled_color;
            } else if hovered.values().any(|v| *v == item.id) {
                color = &settings.menu.hover_color;
                let ph = padding / 2.0;
                cairo.move_to(x - ph, y - ph);
                cairo.line_to(phy.0 as f64 - border_width - ph, y - ph);
                cairo.line_to(phy.0 as f64 - border_width - ph, y + max_label_height + ph);
                cairo.line_to(x - ph, y + max_label_height + ph);
                cairo.line_to(x - ph, y - ph);
                settings.menu.hover_background_color.set(&cairo);
                cairo.fill()?;
            }
            if settings.menu.rtl && has_submenus {
                if item.submenu.is_some() {
                    let dd = sub_width - line_width;
                    cairo.move_to(x + dd, y + max_label_height / 2.0 - dd / 2.0);
                    cairo.rel_line_to(-dd, dd / 2.0);
                    cairo.rel_line_to(dd, dd / 2.0);
                    color.set(&cairo);
                    cairo.set_line_width(line_width);
                    cairo.set_line_cap(LineCap::Round);
                    cairo.stroke()?;
                }
                x += sub_width + 2.0 * padding;
            }
            if has_icons {
                let icon = icon_cache.entry(item.id).or_default();
                icon.update(
                    &item.icon_template,
                    (box_width as i32, box_width as i32),
                    scalef.ceil() as _,
                    &settings.theme,
                    color,
                );
                if let Some(surface) = icon.get() {
                    let pattern = cairo::SurfacePattern::create(&surface);
                    cairo.translate(x, y + max_label_height / 2.0 - box_width / 2.0);
                    cairo.scale(
                        box_width / surface.width() as f64,
                        box_width / surface.height() as f64,
                    );
                    cairo.set_source(&pattern)?;
                    cairo.paint()?;
                    cairo.identity_matrix();
                }
                x += box_width + 2.0 * padding;
            }
            if let Some(tt) = item.toggle_type {
                let y_center = y + (max_label_height / 2.0).floor();
                match tt {
                    SniMenuToggleType::Radio => {
                        cairo.move_to(x + box_width - line_width / 2.0, y_center);
                        cairo.arc(
                            x + box_width / 2.0,
                            y_center,
                            (box_width - line_width) / 2.0,
                            0.0,
                            2.0 * PI,
                        );
                        color.set(&cairo);
                        cairo.set_line_width(line_width);
                        cairo.stroke()?;
                        if item.toggle_active {
                            cairo.move_to(x + box_width - 5.0 * line_width / 2.0, y_center);
                            cairo.arc(
                                x + box_width / 2.0,
                                y_center,
                                (box_width - 5.0 * line_width) / 2.0,
                                0.0,
                                2.0 * PI,
                            );
                            color.set(&cairo);
                            cairo.fill()?;
                        }
                    }
                    SniMenuToggleType::Checkmark => {
                        let dd = box_width - line_width;
                        cairo.move_to(x + line_width / 2.0, y_center - dd / 2.0);
                        cairo.rel_line_to(dd, 0.0);
                        cairo.rel_line_to(0.0, dd);
                        cairo.rel_line_to(-dd, 0.0);
                        cairo.rel_line_to(0.0, -dd);
                        color.set(&cairo);
                        cairo.set_line_width(line_width);
                        cairo.set_line_cap(LineCap::Square);
                        cairo.stroke()?;
                        if item.toggle_active {
                            let line_width = 1.2 * line_width;
                            let inset = 6.0 * line_width / 2.0;
                            cairo.move_to(x + inset, y_center);
                            cairo.line_to(x + box_width / 2.0, y_center + box_width / 2.0 - inset);
                            cairo
                                .line_to(x + box_width - inset, y_center - box_width / 2.0 + inset);
                            color.set(&cairo);
                            cairo.set_line_width(line_width);
                            cairo.set_line_cap(LineCap::Round);
                            cairo.stroke()?;
                        }
                    }
                }
                x += box_width + 2.0 * padding;
            }
            if let Some(label) = &item.label {
                layout.set_text(label);
                cairo.move_to(x, y);
                color.set(&cairo);
                show_layout(&cairo, &layout);
            }
            if !settings.menu.rtl && item.submenu.is_some() {
                x = phy.0 as f64 - padding - border_width - sub_width;
                let dd = sub_width - line_width;
                cairo.move_to(x, y + max_label_height / 2.0 - dd / 2.0);
                cairo.rel_line_to(dd, dd / 2.0);
                cairo.rel_line_to(-dd, dd / 2.0);
                color.set(&cairo);
                cairo.set_line_width(line_width);
                cairo.set_line_cap(LineCap::Round);
                cairo.stroke()?;
            }
            y += max_label_height;
        }
        let y1 = match rows.last() {
            None => border_width + padding / 2.0,
            Some(r) => r.1,
        };
        rows.push((y1, y + padding / 2.0, item.id));
        y += padding;
    }

    // border
    let bw2 = border_width / 2.0;
    cairo.move_to(bw2, bw2);
    cairo.line_to(phy.0 as f64 - bw2, bw2);
    cairo.line_to(phy.0 as f64 - bw2, phy.1 as f64 - bw2);
    cairo.line_to(bw2, phy.1 as f64 - bw2);
    cairo.line_to(bw2, bw2);
    cairo.set_line_width(border_width);
    cairo.set_line_cap(LineCap::Square);
    settings.menu.border_color.set(&cairo);
    cairo.stroke()?;

    drop(cairo);
    surface.flush();
    let buffer = surface.data()?.to_vec();

    let rows = rows
        .into_iter()
        .map(|r| OpenMenuRow {
            y1: (r.0 / wlscale).round() as _,
            y2: (r.1 / wlscale).round() as _,
            menu_id: r.2,
        })
        .collect();

    Ok(Some(RenderedMenu {
        buffer,
        log_space_top: ((border_width + padding / 2.0) / scalef).round() as _,
        log_size: log,
        phy_size: phy,
        rows,
    }))
}
