use {
    error_reporter::Report,
    pangocairo::{cairo, pango::FontDescription},
    serde::{de::Error, Deserialize, Deserializer},
    std::{env::var, fs::File, io::Write, sync::OnceLock},
};

#[derive(Clone, Debug)]
pub struct Settings {
    pub icon: IconSettings,
    pub scale: f64,
    pub menu: MenuSettings,
    pub tooltip: TooltipSettings,
    pub theme: String,
    pub keep_open: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Default)]
pub struct ThemeColor {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

#[derive(Clone, Debug)]
pub struct IconSettings {
    pub color: ThemeColor,
}

#[derive(Clone, Debug)]
pub struct MenuSettings {
    pub font: FontDescription,
    pub color: ThemeColor,
    pub disabled_color: ThemeColor,
    pub background_color: ThemeColor,
    pub hover_color: ThemeColor,
    pub hover_background_color: ThemeColor,
    pub border_color: ThemeColor,
    pub border_width: f64,
    pub padding: f64,
    pub rtl: bool,
}

#[derive(Clone, Debug)]
pub struct TooltipSettings {
    pub font: FontDescription,
    pub color: ThemeColor,
    pub background_color: ThemeColor,
    pub border_color: ThemeColor,
    pub border_width: f64,
    pub padding: f64,
}

impl ThemeColor {
    pub fn set(&self, cairo: &cairo::Context) {
        cairo.set_source_rgba(self.r, self.g, self.b, self.a);
    }
}

impl From<TomlColor> for ThemeColor {
    fn from(value: TomlColor) -> Self {
        Self {
            r: value.r,
            g: value.g,
            b: value.b,
            a: value.a,
        }
    }
}

static SETTINGS: OnceLock<Settings> = OnceLock::new();

pub fn get() -> &'static Settings {
    match SETTINGS.get() {
        None => panic!("settings have not been initialized"),
        Some(s) => s,
    }
}

pub fn init(config: Option<&str>) {
    SETTINGS.get_or_init(|| {
        let path_str;
        let path = if let Some(config) = config {
            config
        } else {
            let config_home = match var("XDG_CONFIG_HOME") {
                Ok(h) => h,
                Err(_) => match var("HOME") {
                    Ok(v) => format!("{v}/.config"),
                    Err(_) => {
                        log::error!("Neither $XDG_CONFIG_HOME nor $HOME are defined");
                        log::warn!("Using default config");
                        return Settings::default();
                    }
                },
            };
            let path = format!("{config_home}/wl-tray-bridge");
            if let Err(e) = std::fs::create_dir_all(&path) {
                log::error!("Could not create {path}: {}", Report::new(e));
                log::warn!("Using default config");
                return Settings::default();
            }
            path_str = format!("{path}/config.toml");
            if let Ok(mut file) = File::options().create_new(true).write(true).open(&path) {
                if let Err(e) = file.write_all(DEFAULT_TOML.as_bytes()) {
                    log::error!(
                        "Could not write default config to {path}: {}",
                        Report::new(e)
                    );
                }
            }
            &path_str
        };
        let c = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                log::error!("Could not read {path}: {}", Report::new(e));
                log::warn!("Using default config");
                return Settings::default();
            }
        };
        deserialize(&c)
    });
}

impl Default for Settings {
    fn default() -> Self {
        deserialize("")
    }
}

pub struct TomlColor {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl<'de> Deserialize<'de> for TomlColor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let Some(s) = s.strip_prefix("#") else {
            return Err(Error::custom("Color must start with a #"));
        };
        let s = s.to_ascii_lowercase();
        if s.chars().any(|c| !matches!(c, '0'..='9' | 'a'..='f')) {
            return Err(Error::custom(
                "Color must only contain characters 0-9a-fA-F",
            ));
        }
        let s = s.as_bytes();
        let nibble = |c: u8| match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            _ => unreachable!(),
        };
        let uno = |c: u8| {
            let v = nibble(c);
            v << 4 | v
        };
        let duo = |c1: u8, c2: u8| nibble(c1) << 4 | nibble(c2);
        let (r, g, b, a) = match s.len() {
            1 => {
                let v = uno(s[0]);
                (v, v, v, 255)
            }
            2 => {
                let v = duo(s[0], s[1]);
                (v, v, v, 255)
            }
            3 => (uno(s[0]), uno(s[1]), uno(s[2]), 255),
            4 => (uno(s[0]), uno(s[1]), uno(s[2]), uno(s[3])),
            6 => (duo(s[0], s[1]), duo(s[2], s[3]), duo(s[4], s[5]), 255),
            8 => (
                duo(s[0], s[1]),
                duo(s[2], s[3]),
                duo(s[4], s[5]),
                duo(s[6], s[7]),
            ),
            _ => return Err(Error::custom("Color must have length 1, 2, 3, 4, 6, or 8")),
        };
        let d = 255.0;
        Ok(TomlColor {
            r: r as f64 / d,
            g: g as f64 / d,
            b: b as f64 / d,
            a: a as f64 / d,
        })
    }
}

fn merge(target: &mut TomlSettings, mut source: TomlSettings) {
    macro_rules! opt {
        ($($ident:ident).+) => {
            let v = source.$($ident).+.take();
            if target.$($ident).+.is_none() {
                target.$($ident).+ = v;
            }
        };
    }
    opt!(scale);
    opt!(keep_open);
    opt!(theme);
    opt!(icon.color);
    opt!(menu.font);
    opt!(menu.color);
    opt!(menu.background_color);
    opt!(menu.hover_color);
    opt!(menu.hover_background_color);
    opt!(menu.disabled_color);
    opt!(menu.border_color);
    opt!(menu.border_width);
    opt!(menu.padding);
    opt!(menu.right_to_left);
    opt!(tooltip.font);
    opt!(tooltip.color);
    opt!(tooltip.background_color);
    opt!(tooltip.border_color);
    opt!(tooltip.border_width);
    opt!(tooltip.padding);
}

const DEFAULT_TOML: &str = include_str!("default.toml");

#[test]
fn empty_deserializes() {
    deserialize("");
}

fn deserialize(s: &str) -> Settings {
    let default = toml::from_str::<TomlSettings>(DEFAULT_TOML).unwrap();
    let mut desired = toml::from_str::<TomlSettings>(s).unwrap_or_else(|e| {
        log::error!("Could not deserialize settings: {}", Report::new(e));
        log::warn!("Falling back to default settings");
        TomlSettings::default()
    });
    merge(&mut desired, default);
    Settings {
        theme: desired.theme.unwrap(),
        keep_open: desired.keep_open.unwrap(),
        icon: IconSettings {
            color: desired.icon.color.unwrap().into(),
        },
        scale: desired.scale.unwrap(),
        menu: MenuSettings {
            font: FontDescription::from_string(&desired.menu.font.unwrap()),
            color: desired.menu.color.unwrap().into(),
            disabled_color: desired.menu.disabled_color.unwrap().into(),
            background_color: desired.menu.background_color.unwrap().into(),
            hover_color: desired.menu.hover_color.unwrap().into(),
            hover_background_color: desired.menu.hover_background_color.unwrap().into(),
            border_color: desired.menu.border_color.unwrap().into(),
            border_width: desired.menu.border_width.unwrap(),
            padding: desired.menu.padding.unwrap(),
            rtl: desired.menu.right_to_left.unwrap(),
        },
        tooltip: TooltipSettings {
            font: FontDescription::from_string(&desired.tooltip.font.unwrap()),
            color: desired.tooltip.color.unwrap().into(),
            background_color: desired.tooltip.background_color.unwrap().into(),
            border_color: desired.tooltip.border_color.unwrap().into(),
            border_width: desired.tooltip.border_width.unwrap(),
            padding: desired.tooltip.padding.unwrap(),
        },
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
struct TomlSettings {
    scale: Option<f64>,
    keep_open: Option<bool>,
    theme: Option<String>,
    #[serde(default)]
    icon: TomlIconSettings,
    #[serde(default)]
    menu: TomlMenuSettings,
    #[serde(default)]
    tooltip: TomlTooltipSettings,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
struct TomlIconSettings {
    color: Option<TomlColor>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
struct TomlMenuSettings {
    font: Option<String>,
    color: Option<TomlColor>,
    background_color: Option<TomlColor>,
    hover_color: Option<TomlColor>,
    hover_background_color: Option<TomlColor>,
    disabled_color: Option<TomlColor>,
    border_color: Option<TomlColor>,
    border_width: Option<f64>,
    padding: Option<f64>,
    right_to_left: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
struct TomlTooltipSettings {
    font: Option<String>,
    color: Option<TomlColor>,
    background_color: Option<TomlColor>,
    border_color: Option<TomlColor>,
    border_width: Option<f64>,
    padding: Option<f64>,
}
