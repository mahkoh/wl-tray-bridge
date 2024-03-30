use {
    crate::{
        settings::{self},
        wayland::{
            scale::{Logical, Scale},
            utils::create_shm_buf_oneshot,
            Singletons,
        },
    },
    pangocairo::{
        cairo::{self, Format, LineCap},
        pango::{self},
        FontMap,
    },
    std::io,
    thiserror::Error,
    wayland_client::protocol::{wl_buffer::WlBuffer, wl_surface::WlSurface},
    wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport,
};

#[derive(Debug, Error)]
pub enum TooltipError {
    #[error(transparent)]
    Cairo(#[from] cairo::Error),
    #[error(transparent)]
    Borrow(#[from] cairo::BorrowError),
    #[error("Could not create a memfd")]
    CreateMemfd(#[source] io::Error),
}

pub struct Tooltip {
    pub buffer: WlBuffer,
    pub surface: WlSurface,
    pub viewport: WpViewport,
    pub log_size: Logical,
}

impl Drop for Tooltip {
    fn drop(&mut self) {
        self.buffer.destroy();
        self.viewport.destroy();
        self.surface.destroy();
    }
}

pub fn create_tooltip(s: &Singletons, scale: Scale, text: &str) -> Result<Tooltip, TooltipError> {
    let (buffer, log) = draw(s, scale, text)?;
    let surface = s.wl_compositor.create_surface(&s.qh, ());
    let viewport = s.wp_viewporter.get_viewport(&surface, &s.qh, ());
    Ok(Tooltip {
        buffer,
        surface,
        viewport,
        log_size: log,
    })
}

fn draw(s: &Singletons, scale: Scale, text: &str) -> Result<(WlBuffer, Logical), TooltipError> {
    let settings = settings::get();
    let wlscale = scale.to_f64();
    let scalef = wlscale * settings.scale;
    let ctx = pango::Context::new();
    ctx.set_font_map(Some(&FontMap::default()));
    let mut font = settings.tooltip.font.clone();
    font.set_size((font.size() as f64 * scalef).round() as _);
    let layout = pango::Layout::new(&ctx);
    layout.set_font_description(Some(&font));
    layout.set_text(text);
    let (width, height) = layout.pixel_size();
    let padding = settings.tooltip.padding * scalef;
    let log = Logical(
        ((width as f64 + 2.0 * padding) / wlscale).round() as i32,
        ((height as f64 + 2.0 * padding) / wlscale).round() as i32,
    );
    let phy = log.to_physical(scale);
    let mut surface = cairo::ImageSurface::create(Format::ARgb32, phy.0, phy.1)?;
    {
        let cairo = cairo::Context::new(&surface)?;

        // background
        let c = settings.tooltip.background_color;
        cairo.set_source_rgba(c.r, c.g, c.b, c.a);
        cairo.paint()?;

        // text
        settings.tooltip.color.set(&cairo);
        cairo.move_to(padding, padding);
        pangocairo::functions::show_layout(&cairo, &layout);

        // border
        let bw = settings.tooltip.border_width * scalef;
        let bw2 = bw / 2.0;
        cairo.move_to(bw2, bw2);
        cairo.line_to(phy.0 as f64 - bw2, bw2);
        cairo.line_to(phy.0 as f64 - bw2, phy.1 as f64 - bw2);
        cairo.line_to(bw2, phy.1 as f64 - bw2);
        cairo.line_to(bw2, bw2);
        cairo.set_line_width(bw);
        cairo.set_line_cap(LineCap::Square);
        settings.tooltip.border_color.set(&cairo);
        cairo.stroke()?;
    }
    surface.flush();
    let data = surface.data()?;
    let buffer = create_shm_buf_oneshot(s, &data, phy.size()).map_err(TooltipError::CreateMemfd)?;
    Ok((buffer, log))
}
