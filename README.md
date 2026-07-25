# wl-tray-bridge

wl-tray-bridge bridges the gap between the [StatusNotifierItem][sni] protocols and
wayland compositors implementing [jay-tray-v1][jtv].

https://github.com/user-attachments/assets/95d8e934-41eb-4b63-8603-5c052cb9e52f

> [!NOTE]
> Support for the experimental [ext-tray-v1][etv] protocol can be enabled with the
> `ext-tray` feature.

[sni]: https://www.freedesktop.org/wiki/Specifications/StatusNotifierItem/
[jtv]: https://github.com/mahkoh/jay-protocols/blob/master/unstable/jay-tray-v1.xml
[etv]: https://gitlab.freedesktop.org/wayland/wayland-protocols/-/merge_requests/355

## Features

- PNG icons
- SVG icons
- Recoloring SVG icons
- Graceful fallback for missing icons
- Fractional scaling
- Transparency
- Menus

## Configuration

wl-tray-bridge is configured with a configuration file normally stored under `~/.config/wl-tray-bridge/config.toml`.

The defaults are

```toml
# This setting applies an additional scale to the entire UI.
# Instead of modifying the individual sizes below, you might get better results by
# changing this value.
scale = 1.0
# The icon theme to use for named icons.
theme = "Hicolor"
# Whether menus should stay open after clicking on an entry.
keep-open = false

# These settings apply to the icons displayed in the tray area.
[icon]
# The color used for SVG icons that allow recoloring.
color = "#285577ff"

# These settings apply to menus.
[menu]
# The font used in menus.
font = "monospace 8"
# The normal font color.
color = "#ffffffff"
# The background color.
background-color = "#000000ff"
# The font color when hovering over an entry.
hover-color = "#ffffffff"
# The background color when hovering over an entry.
hover-background-color = "#285577ff"
# The font color for disabled entries.
disabled-color = "#333333ff"
# The border color.
border-color = "#3f474aff"
# The border width.
border-width = 1.0
# The padding around entries.
padding = 5.0
# Whether sub-menus should be organized from right to left.
right-to-left = true

# These settings apply to tooltips.
[tooltip]
# The font used in tooltips.
font = "monospace 8"
# The font color.
color = "#ffffffff"
# The background color.
background-color = "#000000ff"
# The border color.
border-color = "#3f474aff"
# The border width.
border-width = 1.0
# The padding around the text.
padding = 2.0
```

## License

wl-tray-bridge is free software licensed under the GNU General Public License v3.0.
