# wl-tray-bridge

wl-tray-bridge bridges the gap between the [StatusNotifierItem][sni] protocols and
wayland compositors implementing [ext-tray-v1][etv].

https://github.com/user-attachments/assets/95d8e934-41eb-4b63-8603-5c052cb9e52f

[sni]: https://www.freedesktop.org/wiki/Specifications/StatusNotifierItem/
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
color = "#c8c8c8ff"

# These settings apply to menus.
[menu]
# The font used in menus.
font = "monospace 12"
# The normal font color.
color = "#c8c8c8ff"
# The background color.
background-color = "#4c4c4cff"
# The font color when hovering over an entry.
hover-color = "#c8c8c8ff"
# The background color when hovering over an entry.
hover-background-color = "#00004cff"
# The font color for disabled entries.
disabled-color = "#808080ff"
# The border color.
border-color = "#333333ff"
# The border width.
border-width = 1.0
# The padding around entries.
padding = 5.0
# Whether sub-menus should be organized from right to left.
right-to-left = true

# These settings apply to tooltips.
[tooltip]
# The font used in tooltips.
font = "monospace 12"
# The font color.
color = "#c8c8c8ff"
# The background color.
background-color = "#4c4c4cff"
# The border color.
border-color = "#333333ff"
# The border width.
border-width = 1.0
# The padding around the text.
padding = 2.0
```

## License

wl-tray-bridge is free software licensed under the GNU General Public License v3.0.
