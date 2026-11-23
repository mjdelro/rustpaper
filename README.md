# Rustpaper

A fast, native Rust multi-monitor wallpaper manager for Linux (GNOME / Wayland), inspired by Superpaper and HydraPaper.

## Features
- **Individual Wallpaper per Monitor** — Assign separate images to each display with `Fill (Crop)` (plus focal X/Y crop offset), `Fit (Letterbox)`, `Stretch`, and `Center` modes.
- **Physical Spanning (PPI + Bezel Compensation)** — Span a single wallpaper across mixed-resolution, mixed-size, and mixed-orientation monitors (e.g. portrait + landscape) with per-monitor diagonal inch calibration, bezel width in millimeters, manual alignment nudge, zoom, and pan.
- **Live Interactive Layout Preview** — Visualizes your exact GNOME monitor arrangement (including portrait rotations and physical bezel/PPI geometry) with live wallpaper thumbnails.
- **Separate GNOME Light & Dark Mode Wallpapers** — Configure independent wallpaper setups for `picture-uri` and `picture-uri-dark`.
- **Profiles & Slideshow Rotation** — Save/switch named profiles and rotate per-monitor or spanned folders automatically in-app or via the headless daemon.

## Usage

```bash
# Launch the GUI
cargo run --release

# Inspect detected monitors & stitched canvas geometry
cargo run --release -- list-monitors

# Apply the active (or named) profile headlessly
cargo run --release -- apply
cargo run --release -- apply "Work"

# Advance slideshow to next image(s) and apply
cargo run --release -- next

# Run headless slideshow daemon
cargo run --release -- daemon
```

Config is stored at `~/.config/rustpaper/config.toml` and generated stitched wallpapers are cached in `~/.cache/rustpaper/`.
