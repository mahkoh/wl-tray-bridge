use {
    crate::{
        settings::ThemeColor,
        sni::IconFrames,
        wayland::{utils::create_shm_buf_oneshot, Singletons},
    },
    ahash::{AHashMap, AHashSet},
    error_reporter::Report,
    ini::{Ini, ParseError},
    pangocairo::cairo::{self},
    png::Transformations,
    resvg::{
        tiny_skia::{PixmapMut, Transform},
        usvg::{self, Options, Tree},
    },
    std::{
        env::var,
        io, mem,
        os::unix::ffi::OsStrExt,
        path::{Path, PathBuf},
        str::FromStr,
        sync::{
            atomic::{AtomicUsize, Ordering::Relaxed},
            Arc, LazyLock,
        },
    },
    thiserror::Error,
    wayland_client::protocol::wl_buffer::WlBuffer,
};

static VERSION: AtomicUsize = AtomicUsize::new(1);

#[derive(Default)]
pub struct IconTemplate {
    version: usize,
    name: Option<Arc<String>>,
    frames: Option<IconFrames>,
    path: Option<Arc<String>>,
    themes: AHashMap<String, Vec<Theme>>,
}

#[derive(Default)]
struct IconVersion {
    version: usize,
    size: (i32, i32),
    scale: i32,
    color: ThemeColor,
}

pub struct BufferIconFrame {
    pub buffer: WlBuffer,
}

#[derive(Default)]
pub struct BufferIcon {
    version: IconVersion,
    buffer: Option<(BufferIconFrame, (i32, i32))>,
}

#[derive(Default)]
pub struct CairoIcon {
    version: IconVersion,
    surface: Option<cairo::ImageSurface>,
}

impl From<WlBuffer> for BufferIconFrame {
    fn from(value: WlBuffer) -> Self {
        Self { buffer: value }
    }
}

impl Drop for BufferIconFrame {
    fn drop(&mut self) {
        self.buffer.destroy();
    }
}

impl IconTemplate {
    pub fn is_some(&self) -> bool {
        self.frames.is_some() || self.name.is_some()
    }

    pub fn update_name(&mut self, name: Option<&Arc<String>>, path: Option<&Arc<String>>) {
        if (self.name.as_ref(), self.path.as_ref()) == (name, path) {
            return;
        }
        self.version = VERSION.fetch_add(1, Relaxed);
        self.name = name.cloned();
        if self.path.as_ref() != path {
            self.path = path.cloned();
            self.themes.clear();
            if let Some(path) = path {
                parse_themes_in_dir(Path::new(&**path), &mut self.themes);
            }
        }
    }

    pub fn update_frames(&mut self, mut frames: Option<&IconFrames>) {
        if let Some(f) = frames {
            if f.frames.is_empty() {
                frames = None;
            }
        }
        if self.frames.as_ref() == frames {
            return;
        }
        self.version = VERSION.fetch_add(1, Relaxed);
        self.frames = frames.cloned();
    }

    fn realize(
        &self,
        size: (i32, i32),
        scale: i32,
        theme: &str,
        color: &ThemeColor,
    ) -> Option<(Vec<u8>, (i32, i32))> {
        if let Some(name) = &self.name {
            let custom_themes = self.path.as_ref().map(|dir| CustomThemes {
                dir,
                themes: &self.themes,
            });
            let res = name_to_bytes(name, size, scale, theme, custom_themes, color);
            if let Some(res) = res {
                return Some(res);
            }
        }
        if let Some(frames) = &self.frames {
            let mut min_dist = u64::MAX;
            let mut best_frame = 0;
            for (idx, frame) in frames.frames.iter().enumerate() {
                let dx = (size.0 as u64).abs_diff(frame.size.0 as u64);
                let dy = (size.1 as u64).abs_diff(frame.size.1 as u64);
                let dist = dx * dx + dy * dy;
                if dist < min_dist {
                    min_dist = dist;
                    best_frame = idx;
                }
            }
            let frame = &frames.frames[best_frame];
            let mut bytes = frame.bytes.clone();
            let mut chunks = bytes.chunks_mut(4);
            while let Some([r, g, b, a]) = chunks.next() {
                mem::swap(r, a);
                mem::swap(g, b);
                *r = (*r as f32 * *a as f32 / 255.0) as u8;
                *g = (*g as f32 * *a as f32 / 255.0) as u8;
                *b = (*b as f32 * *a as f32 / 255.0) as u8;
            }
            return Some((bytes, frame.size));
        }
        if self.name.is_none() && self.frames.is_none() {
            return None;
        }
        let data = match render_svg(include_bytes!("fallback.svg"), size, color) {
            Ok(d) => d,
            Err(e) => {
                log::error!("Could not render fallback: {}", Report::new(e));
                vec![255; (size.0 * size.1) as usize]
            }
        };
        Some((data, size))
    }
}

impl IconVersion {
    pub fn update(
        &mut self,
        template: &IconTemplate,
        size: (i32, i32),
        scale: i32,
        color: &ThemeColor,
    ) -> bool {
        if self.version == template.version {
            if template.frames.is_some() {
                return true;
            }
            if (self.size, self.scale, &self.color) == (size, scale, color) {
                return true;
            }
        }
        self.version = template.version;
        self.size = size;
        self.scale = scale;
        self.color = *color;
        false
    }
}

#[derive(Debug, Error)]
enum BufferIconError {
    #[error("Could not create memfd")]
    CreateShmBuffer(#[source] io::Error),
}

impl BufferIcon {
    pub fn get(&self) -> Option<&(BufferIconFrame, (i32, i32))> {
        self.buffer.as_ref()
    }

    pub fn update(
        &mut self,
        template: &IconTemplate,
        size: (i32, i32),
        scale: i32,
        theme: &str,
        color: &ThemeColor,
        s: &Singletons,
    ) {
        if let Err(e) = self.try_update(template, size, scale, theme, color, s) {
            log::error!("Could not update buffers: {}", Report::new(e));
        }
    }

    fn try_update(
        &mut self,
        template: &IconTemplate,
        size: (i32, i32),
        scale: i32,
        theme: &str,
        color: &ThemeColor,
        s: &Singletons,
    ) -> Result<(), BufferIconError> {
        if self.version.update(template, size, scale, color) {
            return Ok(());
        }
        self.buffer.take();
        let Some((contents, size)) = template.realize(size, scale, theme, color) else {
            return Ok(());
        };
        let buffer =
            create_shm_buf_oneshot(s, &contents, size).map_err(BufferIconError::CreateShmBuffer)?;
        self.buffer = Some((buffer.into(), size));
        Ok(())
    }
}

impl CairoIcon {
    pub fn get(&self) -> Option<cairo::ImageSurface> {
        self.surface.clone()
    }

    pub fn update(
        &mut self,
        template: &IconTemplate,
        size: (i32, i32),
        scale: i32,
        theme: &str,
        color: &ThemeColor,
    ) {
        if self.version.update(template, size, scale, color) {
            return;
        }
        self.surface.take();
        let Some((rgba, size)) = template.realize(size, scale, theme, color) else {
            return;
        };
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, size.0, size.1);
        let mut surface = match surface {
            Ok(s) => s,
            Err(e) => {
                log::error!("Could not create cairo surface: {}", Report::new(e));
                return;
            }
        };
        {
            let mut data = match surface.data() {
                Ok(d) => d,
                Err(e) => {
                    log::error!("Could not write cairo surface data: {}", Report::new(e));
                    return;
                }
            };
            let len = data.len();
            data.copy_from_slice(&rgba[..len]);
        }
        surface.flush();
        self.surface = Some(surface);
    }
}

fn name_to_bytes(
    name: &str,
    size: (i32, i32),
    scale: i32,
    theme: &str,
    custom_themes: Option<CustomThemes<'_>>,
    color: &ThemeColor,
) -> Option<(Vec<u8>, (i32, i32))> {
    let lookup = find_icon(name, size.0.max(size.1), scale, theme, custom_themes)?;
    let contents = match std::fs::read(&lookup.path) {
        Ok(c) => c,
        Err(e) => {
            log::error!(
                "Could not read {}: {}",
                lookup.path.display(),
                Report::new(e)
            );
            return None;
        }
    };
    let ext = lookup.path.extension()?;
    let (mut contents, size) = match ext.as_bytes() {
        b"svg" => match render_svg(&contents, size, color) {
            Ok(b) => (b, size),
            Err(e) => {
                log::error!("Could not render svg: {}", Report::new(e));
                return None;
            }
        },
        b"png" => match render_png(&contents) {
            Ok(b) => b,
            Err(e) => {
                log::error!("Could not render png: {}", Report::new(e));
                return None;
            }
        },
        _ => return None,
    };
    let mut chunks = contents.chunks_mut(4);
    while let Some([r, g, b, a]) = chunks.next() {
        // Convert to premultiplied BGRA.
        mem::swap(r, b);
        *r = (*r as f32 * *a as f32 / 255.0) as u8;
        *g = (*g as f32 * *a as f32 / 255.0) as u8;
        *b = (*b as f32 * *a as f32 / 255.0) as u8;
    }
    Some((contents, size))
}

fn render_svg(
    contents: &[u8],
    size: (i32, i32),
    color: &ThemeColor,
) -> Result<Vec<u8>, usvg::Error> {
    let map = |c: f64| (c * 255.0).round();
    let stylesheet = format!(
        "* {{ color: rgb({} {} {} {}); }}",
        map(color.r),
        map(color.g),
        map(color.b),
        map(color.a)
    );
    let mut options = Options::default();
    options.style_sheet = Some(stylesheet);
    let tree = Tree::from_data(contents, &options)?;
    let mut res = vec![0; (size.0 * size.1 * 4) as usize];
    let mut pixmap = PixmapMut::from_bytes(&mut res, size.0 as _, size.1 as _)
        .expect("Could not create PixmapMut");
    let actual = tree.size();
    let transform = Transform::from_scale(
        size.0 as f32 / actual.width(),
        size.1 as f32 / actual.height(),
    );
    resvg::render(&tree, transform, &mut pixmap);
    Ok(res)
}

pub fn render_png(mut contents: &[u8]) -> Result<(Vec<u8>, (i32, i32)), png::DecodingError> {
    let mut decoder = png::Decoder::new(&mut contents);
    decoder.set_transformations(Transformations::STRIP_16 | Transformations::ALPHA);
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf)?;
    Ok((buf, (info.width as _, info.height as _)))
}

#[derive(Debug)]
struct IconLookup {
    path: PathBuf,
}

struct CustomThemes<'a> {
    dir: &'a str,
    themes: &'a AHashMap<String, Vec<Theme>>,
}

fn find_icon(
    name: &str,
    size: i32,
    scale: i32,
    theme: &str,
    custom_themes: Option<CustomThemes<'_>>,
) -> Option<IconLookup> {
    if name.ends_with(".png") || name.ends_with("svg") {
        if let Ok(m) = std::fs::metadata(name) {
            if m.is_file() {
                return Some(IconLookup {
                    path: Path::new(name).to_path_buf(),
                });
            }
        }
    }
    if let Some(custom) = custom_themes {
        let dir = Path::new(custom.dir);
        let res = find_icon_within([dir], custom.themes, name, size, scale, theme);
        if res.is_some() {
            return res;
        }
    }
    find_icon_within(
        BASE_DIRS.iter().map(|d| &**d),
        &THEMES,
        name,
        size,
        scale,
        theme,
    )
}

fn find_icon_within<'a, I>(
    base_dirs: I,
    themes: &AHashMap<String, Vec<Theme>>,
    name: &str,
    size: i32,
    scale: i32,
    theme: &str,
) -> Option<IconLookup>
where
    I: IntoIterator<Item = &'a Path>,
{
    let mut searched_themes = AHashSet::new();
    let mut res = find_icon_helper(themes, name, size, scale, theme, &mut searched_themes);
    if res.is_none() {
        res = find_icon_helper(themes, name, size, scale, "Hicolor", &mut searched_themes);
    }
    if res.is_none() {
        for dir in base_dirs {
            res = find_icon_in_dir(dir, "", name).map(|path| IconLookup { path });
            if res.is_some() {
                break;
            }
        }
    }
    res
}

fn find_icon_helper<'a>(
    themes: &'a AHashMap<String, Vec<Theme>>,
    name: &str,
    size: i32,
    scale: i32,
    theme_name: &'a str,
    searched_themes: &mut AHashSet<&'a str>,
) -> Option<IconLookup> {
    if !searched_themes.insert(theme_name) {
        return None;
    }
    for theme in themes.get(theme_name)? {
        let res = lookup_icon(name, size, scale, theme);
        if res.is_some() {
            return res;
        }
        for parent in &theme.inherits {
            let res = find_icon_helper(themes, name, size, scale, parent, searched_themes);
            if res.is_some() {
                return res;
            }
        }
    }
    None
}

fn lookup_icon(name: &str, size: i32, scale: i32, theme: &Theme) -> Option<IconLookup> {
    for dir in &theme.directories {
        if let Some(variant) = theme.variants.get(dir) {
            if variant.permits_size(size, scale) {
                if let Some(path) = find_icon_in_dir(&theme.dir, dir, name) {
                    return Some(IconLookup { path });
                }
            }
        }
    }
    let mut min_size = i32::MAX;
    let mut closest = None;
    for dir in &theme.directories {
        if let Some(variant) = theme.variants.get(dir) {
            let dist = variant.distance(size, scale);
            if dist >= min_size {
                continue;
            }
            if let Some(path) = find_icon_in_dir(&theme.dir, dir, name) {
                min_size = dist;
                closest = Some(IconLookup { path });
            }
        }
    }
    closest
}

fn find_icon_in_dir(dir: &Path, subdir: &str, name: &str) -> Option<PathBuf> {
    const EXTENSIONS: [&str; 2] = ["svg", "png"];
    for ext in EXTENSIONS {
        let path = dir.join(format!("./{subdir}/{name}.{ext}"));
        if path.exists() {
            return Some(path);
        }
    }
    None
}

impl Variant {
    fn permits_size(&self, size: i32, scale: i32) -> bool {
        if self.scale != scale {
            return false;
        }
        match self.ty {
            VariantType::Threshold => {
                self.size - self.threshold <= size && size <= self.size + self.threshold
            }
            VariantType::Scalable => self.min_size <= size && size <= self.max_size,
            VariantType::Fixed => self.size == size,
        }
    }

    fn distance(&self, size: i32, scale: i32) -> i32 {
        match self.ty {
            VariantType::Threshold => {
                if size * scale < (self.size - self.threshold) * self.scale {
                    return self.min_size * self.scale - size * scale;
                }
                if size * size > (self.size + self.threshold) * self.scale {
                    return size * size - self.max_size * self.scale;
                }
                0
            }
            VariantType::Scalable => {
                if size * scale < self.min_size * self.scale {
                    return self.min_size * self.scale - size * scale;
                }
                if size * scale > self.max_size * self.scale {
                    return size * scale - self.max_size * self.scale;
                }
                0
            }
            VariantType::Fixed => (self.size * self.scale - size * scale).abs(),
        }
    }
}

static THEMES: LazyLock<AHashMap<String, Vec<Theme>>> = LazyLock::new(|| {
    let mut themes = AHashMap::<_, Vec<_>>::new();
    for dir in &*BASE_DIRS {
        parse_themes_in_dir(dir, &mut themes);
    }
    themes
});

fn parse_themes_in_dir(dir: &Path, out: &mut AHashMap<String, Vec<Theme>>) {
    let Ok(mut dir) = dir.read_dir() else {
        return;
    };
    while let Some(Ok(dir)) = dir.next() {
        let path = dir.path();
        let res = parse_theme(&path);
        if let Some(res) = res.transpose() {
            match res {
                Ok(theme) => {
                    out.entry(theme.name.clone()).or_default().push(theme);
                }
                Err(e) => {
                    log::debug!(
                        "Could not parse theme in {}: {}",
                        path.display(),
                        Report::new(e)
                    );
                }
            }
        }
    }
}

#[derive(Debug)]
struct Theme {
    name: String,
    dir: PathBuf,
    _comment: Option<String>,
    inherits: Vec<String>,
    directories: Vec<String>,
    variants: AHashMap<String, Variant>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum VariantType {
    Threshold,
    Scalable,
    Fixed,
}

#[derive(Debug)]
struct Variant {
    size: i32,
    scale: i32,
    ty: VariantType,
    max_size: i32,
    min_size: i32,
    threshold: i32,
}

#[derive(Debug, Error)]
enum ThemeError {
    #[error("Could not parse the theme file")]
    Parse(#[source] ParseError),
    #[error("The theme has no name")]
    NoName,
}

fn parse_theme(dir: &Path) -> Result<Option<Theme>, ThemeError> {
    let file = dir.join("index.theme");
    let Ok(theme) = std::fs::read_to_string(&file) else {
        return Ok(None);
    };
    let mut ini = Ini::load_from_str(&theme).map_err(ThemeError::Parse)?;
    let Some(desc) = ini.delete(Some("Icon Theme")) else {
        return Ok(None);
    };
    let split = |name: &str| {
        desc.get(name)
            .unwrap_or_default()
            .split(",")
            .map(ToOwned::to_owned)
    };
    let mut theme = Theme {
        name: desc.get("Name").ok_or(ThemeError::NoName)?.to_string(),
        dir: dir.to_owned(),
        _comment: desc.get("Comment").map(ToOwned::to_owned),
        inherits: split("Inherits").collect(),
        directories: split("Directories")
            .chain(split("ScaledDirectories"))
            .collect(),
        variants: Default::default(),
    };
    for (section, props) in ini.iter() {
        let Some(section) = section else {
            continue;
        };
        let Some(size) = props.get("Size") else {
            continue;
        };
        let Ok(size) = i32::from_str(size) else {
            continue;
        };
        let ty = match props.get("Type") {
            None | Some("Threshold") => VariantType::Threshold,
            Some("Scalable") => VariantType::Scalable,
            Some("Fixed") => VariantType::Fixed,
            Some(ty) => {
                log::error!("In {}: Unknown Type {}", dir.display(), ty);
                continue;
            }
        };
        macro_rules! int {
            ($name:expr, $default:expr) => {
                match props.get($name) {
                    None => $default,
                    Some(v) => match i32::from_str(v) {
                        Ok(v) => v,
                        Err(e) => {
                            log::error!(
                                "In {}: Could not parse {}: {}",
                                dir.display(),
                                $name,
                                Report::new(e)
                            );
                            continue;
                        }
                    },
                }
            };
        }
        theme.variants.insert(
            section.to_string(),
            Variant {
                size,
                scale: int!("Scale", 1),
                ty,
                max_size: int!("MaxSize", size),
                min_size: int!("MinSize", size),
                threshold: int!("Threshold", 2),
            },
        );
    }
    Ok(Some(theme))
}

static BASE_DIRS: LazyLock<Vec<PathBuf>> = LazyLock::new(|| {
    let mut dirs = vec![];
    dirs.push("$HOME/.icons".to_string());
    if let Ok(data_home) = var("XDG_DATA_HOME") {
        dirs.push(format!("{data_home}/icons"));
    } else {
        dirs.push("$HOME/.local/share/icons".to_string());
    }
    dirs.push("/usr/share/pixmaps".to_string());
    if let Ok(data_dirs) = var("XDG_DATA_DIRS") {
        for dir in data_dirs.split(":") {
            dirs.push(format!("{dir}/icons"));
        }
    } else {
        dirs.push("/usr/local/share/icons".to_string());
        dirs.push("/usr/share/icons".to_string());
    }
    dirs.into_iter()
        .flat_map(|d| shellexpand::full(&d).ok().map(|s| s.into_owned()))
        .map(PathBuf::from)
        .collect()
});
