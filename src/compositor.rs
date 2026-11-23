use crate::config::{
    FitMode, MonitorPhysicalConfig, Profile, ThemeWallpaperSet, WallpaperMode,
};
use crate::monitors::{DesktopLayout, Monitor};
use anyhow::{Context, Result};
use image::imageops::{self, FilterType};
use image::{DynamicImage, GenericImageView, Rgb, RgbImage};
use std::collections::BTreeMap;
use std::path::Path;

/// Physical bounding rectangle of a monitor in millimeters (used for PPI + bezel spanning).
#[derive(Debug, Clone)]
pub struct PhysicalMonitorRect {
    pub x_mm: f64,
    pub y_mm: f64,
    pub w_mm: f64,
    pub h_mm: f64,
}

#[derive(Debug, Clone)]
pub struct PhysicalLayout {
    pub rects: Vec<PhysicalMonitorRect>,
    pub total_w: f64,
    pub total_h: f64,
}

/// Computes physical millimeter coordinates for each monitor, accounting for
/// PPI differences (if `ppi_correction`) and bezel gaps (if `bezel_correction`).
pub fn compute_physical_layout(
    layout: &DesktopLayout,
    phys_cfg: &BTreeMap<String, MonitorPhysicalConfig>,
    ppi_correction: bool,
    bezel_correction: bool,
) -> PhysicalLayout {
    // Reference PPI from the primary monitor (or first monitor)
    let primary = layout
        .monitors
        .iter()
        .find(|m| m.primary)
        .unwrap_or(&layout.monitors[0]);
    let primary_diag = phys_cfg
        .get(&primary.connector)
        .map(|c| c.diagonal_inches)
        .unwrap_or_else(|| primary.guessed_diagonal_inches())
        .max(5.0);
    let primary_ppi = ((primary.physical_width as f64).hypot(primary.physical_height as f64))
        / primary_diag;
    let ref_mm_per_logical_px = 25.4 / (primary_ppi / primary.scale.max(0.25));

    struct TempRect {
        monitor_idx: usize,
        x_mm: f64,
        y_mm: f64,
        w_mm: f64,
        h_mm: f64,
        mm_per_lpx_y: f64,
        bezel_r: f64,
        bezel_b: f64,
    }

    let mut placed: Vec<TempRect> = Vec::with_capacity(layout.monitors.len());

    for (idx, m) in layout.monitors.iter().enumerate() {
        let cfg = phys_cfg.get(&m.connector);
        let diag = cfg
            .map(|c| c.diagonal_inches)
            .unwrap_or_else(|| m.guessed_diagonal_inches())
            .max(5.0);
        let bezel = cfg.map(|c| c.bezel_mm.clone()).unwrap_or_default();
        let (off_x, off_y) = cfg.map(|c| (c.offset_x_mm, c.offset_y_mm)).unwrap_or((0.0, 0.0));

        let (w_mm, h_mm) = if ppi_correction {
            let ppi = ((m.physical_width as f64).hypot(m.physical_height as f64)) / diag;
            ((m.physical_width as f64 / ppi) * 25.4, (m.physical_height as f64 / ppi) * 25.4)
        } else {
            (
                m.width as f64 * ref_mm_per_logical_px,
                m.height as f64 * ref_mm_per_logical_px,
            )
        };

        let mm_per_lpx_y = h_mm / (m.height.max(1) as f64);

        // Try to anchor flush against an already-placed neighbor in logical space
        let mut x_mm = (m.x - layout.min_x) as f64 * ref_mm_per_logical_px;
        let mut y_mm = (m.y - layout.min_y) as f64 * ref_mm_per_logical_px;

        // Horizontal neighbor directly to the left
        if let Some(left_neighbor) = placed.iter().find(|p| {
            let lm = &layout.monitors[p.monitor_idx];
            let touches_x = ((lm.x + lm.width as i32) - m.x).abs() <= 4;
            let overlaps_y = lm.y < (m.y + m.height as i32) && m.y < (lm.y + lm.height as i32);
            touches_x && overlaps_y
        }) {
            let lm = &layout.monitors[left_neighbor.monitor_idx];
            let gap = if bezel_correction {
                left_neighbor.bezel_r + bezel.left
            } else {
                0.0
            };
            x_mm = left_neighbor.x_mm + left_neighbor.w_mm + gap;

            // Preserve top or bottom alignment if they align in logical space
            let top_aligned = (lm.y - m.y).abs() <= 4;
            let bottom_aligned =
                ((lm.y + lm.height as i32) - (m.y + m.height as i32)).abs() <= 4;
            if top_aligned {
                y_mm = left_neighbor.y_mm;
            } else if bottom_aligned {
                y_mm = left_neighbor.y_mm + left_neighbor.h_mm - h_mm;
            } else {
                y_mm = left_neighbor.y_mm + (m.y - lm.y) as f64 * left_neighbor.mm_per_lpx_y;
            }
        } else if let Some(top_neighbor) = placed.iter().find(|p| {
            let tm = &layout.monitors[p.monitor_idx];
            let touches_y = ((tm.y + tm.height as i32) - m.y).abs() <= 4;
            let overlaps_x = tm.x < (m.x + m.width as i32) && m.x < (tm.x + tm.width as i32);
            touches_y && overlaps_x
        }) {
            let tm = &layout.monitors[top_neighbor.monitor_idx];
            let gap = if bezel_correction {
                top_neighbor.bezel_b + bezel.top
            } else {
                0.0
            };
            y_mm = top_neighbor.y_mm + top_neighbor.h_mm + gap;
            let left_aligned = (tm.x - m.x).abs() <= 4;
            let right_aligned =
                ((tm.x + tm.width as i32) - (m.x + m.width as i32)).abs() <= 4;
            if left_aligned {
                x_mm = top_neighbor.x_mm;
            } else if right_aligned {
                x_mm = top_neighbor.x_mm + top_neighbor.w_mm - w_mm;
            } else {
                x_mm = top_neighbor.x_mm + (m.x - tm.x) as f64 * (top_neighbor.w_mm / tm.width.max(1) as f64);
            }
        }

        placed.push(TempRect {
            monitor_idx: idx,
            x_mm: x_mm + off_x,
            y_mm: y_mm + off_y,
            w_mm,
            h_mm,
            mm_per_lpx_y,
            bezel_r: bezel.right,
            bezel_b: bezel.bottom,
        });
    }

    let min_x = placed.iter().map(|r| r.x_mm).fold(f64::INFINITY, f64::min);
    let min_y = placed.iter().map(|r| r.y_mm).fold(f64::INFINITY, f64::min);
    let max_x = placed
        .iter()
        .map(|r| r.x_mm + r.w_mm)
        .fold(f64::NEG_INFINITY, f64::max);
    let max_y = placed
        .iter()
        .map(|r| r.y_mm + r.h_mm)
        .fold(f64::NEG_INFINITY, f64::max);

    let rects = placed
        .into_iter()
        .map(|r| PhysicalMonitorRect {
            x_mm: r.x_mm - min_x,
            y_mm: r.y_mm - min_y,
            w_mm: r.w_mm,
            h_mm: r.h_mm,
        })
        .collect();

    PhysicalLayout {
        rects,
        total_w: (max_x - min_x).max(1.0),
        total_h: (max_y - min_y).max(1.0),
    }
}

/// Renders a single monitor image into a `(target_w, target_h)` RGB buffer according to `fit_mode`.
pub fn render_single_monitor(
    src: &DynamicImage,
    target_w: u32,
    target_h: u32,
    fit_mode: FitMode,
    offset_x: f32,
    offset_y: f32,
    bg_color: [u8; 3],
    filter: FilterType,
) -> RgbImage {
    let (sw, sh) = src.dimensions();
    if sw == 0 || sh == 0 || target_w == 0 || target_h == 0 {
        return RgbImage::from_pixel(target_w.max(1), target_h.max(1), Rgb(bg_color));
    }

    match fit_mode {
        FitMode::Fill => {
            let scale = (target_w as f64 / sw as f64).max(target_h as f64 / sh as f64);
            let crop_w = ((target_w as f64 / scale).round() as u32).clamp(1, sw);
            let crop_h = ((target_h as f64 / scale).round() as u32).clamp(1, sh);
            let max_x = sw.saturating_sub(crop_w);
            let max_y = sh.saturating_sub(crop_h);
            let fx = ((offset_x.clamp(-1.0, 1.0) + 1.0) * 0.5) as f64;
            let fy = ((offset_y.clamp(-1.0, 1.0) + 1.0) * 0.5) as f64;
            let cx = ((max_x as f64) * fx).round() as u32;
            let cy = ((max_y as f64) * fy).round() as u32;

            let cropped = src.crop_imm(cx, cy, crop_w, crop_h);
            imageops::resize(&cropped.to_rgb8(), target_w, target_h, filter)
        }
        FitMode::Fit => {
            let scale = (target_w as f64 / sw as f64).min(target_h as f64 / sh as f64);
            let new_w = ((sw as f64 * scale).round() as u32).clamp(1, target_w);
            let new_h = ((sh as f64 * scale).round() as u32).clamp(1, target_h);
            let resized = imageops::resize(&src.to_rgb8(), new_w, new_h, filter);
            let mut out = RgbImage::from_pixel(target_w, target_h, Rgb(bg_color));
            let ox = (target_w - new_w) / 2;
            let oy = (target_h - new_h) / 2;
            imageops::replace(&mut out, &resized, ox as i64, oy as i64);
            out
        }
        FitMode::Stretch => imageops::resize(&src.to_rgb8(), target_w, target_h, filter),
        FitMode::Center => {
            let mut out = RgbImage::from_pixel(target_w, target_h, Rgb(bg_color));
            let rgb = src.to_rgb8();
            let copy_w = sw.min(target_w);
            let copy_h = sh.min(target_h);
            let src_x = (sw - copy_w) / 2;
            let src_y = (sh - copy_h) / 2;
            let dst_x = (target_w - copy_w) / 2;
            let dst_y = (target_h - copy_h) / 2;
            let sub = imageops::crop_imm(&rgb, src_x, src_y, copy_w, copy_h).to_image();
            imageops::replace(&mut out, &sub, dst_x as i64, dst_y as i64);
            out
        }
    }
}

/// Composes the full stitched canvas for a `ThemeWallpaperSet` and saves it to `output_path`.
pub fn compose_and_save(
    layout: &DesktopLayout,
    profile: &Profile,
    theme_set: &ThemeWallpaperSet,
    phys_cfg: &BTreeMap<String, MonitorPhysicalConfig>,
    output_path: &Path,
) -> Result<()> {
    let mut canvas = RgbImage::from_pixel(
        layout.canvas_width,
        layout.canvas_height,
        Rgb(theme_set.background_color),
    );

    match profile.mode {
        WallpaperMode::PerMonitor => {
            for monitor in &layout.monitors {
                let Some(mw) = theme_set.per_monitor.get(&monitor.connector) else {
                    continue;
                };
                let Some(path) = &mw.image_path else {
                    continue;
                };
                if !path.exists() {
                    continue;
                }
                let src = image::open(path)
                    .with_context(|| format!("Failed to open image {}", path.display()))?;
                let (tx, ty, tw, th) = layout.monitor_canvas_rect(monitor);
                let rendered = render_single_monitor(
                    &src,
                    tw,
                    th,
                    mw.fit_mode,
                    mw.offset_x,
                    mw.offset_y,
                    theme_set.background_color,
                    FilterType::Lanczos3,
                );
                imageops::replace(&mut canvas, &rendered, tx as i64, ty as i64);
            }
        }
        WallpaperMode::Spanned => {
            let spanned = &theme_set.spanned;
            if let Some(path) = &spanned.image_path {
                if path.exists() {
                    let src = image::open(path).with_context(|| {
                        format!("Failed to open spanned image {}", path.display())
                    })?;
                    let (sw, sh) = src.dimensions();
                    let phys_layout = compute_physical_layout(
                        layout,
                        phys_cfg,
                        spanned.ppi_correction,
                        spanned.bezel_correction,
                    );

                    // Compute how the physical wall maps into the source image (cover + zoom + pan)
                    let wall_aspect = phys_layout.total_w / phys_layout.total_h;
                    let src_aspect = sw as f64 / sh as f64;
                    let zoom = spanned.zoom.clamp(1.0, 4.0) as f64;

                    let (view_w, view_h) = if src_aspect > wall_aspect {
                        let vh = sh as f64 / zoom;
                        let vw = vh * wall_aspect;
                        (vw, vh)
                    } else {
                        let vw = sw as f64 / zoom;
                        let vh = vw / wall_aspect;
                        (vw, vh)
                    };

                    let max_pan_x = ((sw as f64) - view_w).max(0.0);
                    let max_pan_y = ((sh as f64) - view_h).max(0.0);
                    let fx = ((spanned.pan_x.clamp(-1.0, 1.0) + 1.0) * 0.5) as f64;
                    let fy = ((spanned.pan_y.clamp(-1.0, 1.0) + 1.0) * 0.5) as f64;
                    let view_x = max_pan_x * fx;
                    let view_y = max_pan_y * fy;

                    for (monitor, prect) in layout.monitors.iter().zip(phys_layout.rects.iter()) {
                        let (tx, ty, tw, th) = layout.monitor_canvas_rect(monitor);
                        let nx0 = prect.x_mm / phys_layout.total_w;
                        let ny0 = prect.y_mm / phys_layout.total_h;
                        let nw = prect.w_mm / phys_layout.total_w;
                        let nh = prect.h_mm / phys_layout.total_h;

                        let sx = (view_x + nx0 * view_w).round().clamp(0.0, (sw - 1) as f64) as u32;
                        let sy = (view_y + ny0 * view_h).round().clamp(0.0, (sh - 1) as f64) as u32;
                        let cw = (nw * view_w).round().max(1.0).min((sw - sx) as f64) as u32;
                        let ch = (nh * view_h).round().max(1.0).min((sh - sy) as f64) as u32;

                        let cropped = src.crop_imm(sx, sy, cw, ch);
                        let resized =
                            imageops::resize(&cropped.to_rgb8(), tw, th, FilterType::Lanczos3);
                        imageops::replace(&mut canvas, &resized, tx as i64, ty as i64);
                    }
                }
            }
        }
    }

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    canvas
        .save(output_path)
        .with_context(|| format!("Failed to save stitched wallpaper to {}", output_path.display()))?;
    Ok(())
}

/// Helper to summarize a monitor for display.
pub fn monitor_summary(m: &Monitor) -> String {
    let primary_tag = if m.primary { " ★" } else { "" };
    format!(
        "{}{} — {} ({}×{} {})",
        m.connector,
        primary_tag,
        m.display_name,
        m.width,
        m.height,
        if m.is_portrait() { "Portrait" } else { "Landscape" }
    )
}
