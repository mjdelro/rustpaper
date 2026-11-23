use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use zbus::blocking::Connection;
use zbus::zvariant::OwnedValue;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Monitor {
    pub connector: String,
    pub display_name: String,
    pub vendor: String,
    pub product: String,
    /// Logical X position in desktop space
    pub x: i32,
    /// Logical Y position in desktop space
    pub y: i32,
    /// Logical width (after rotation & scale)
    pub width: u32,
    /// Logical height (after rotation & scale)
    pub height: u32,
    /// Physical pixel width (after rotation, before scale)
    pub physical_width: u32,
    /// Physical pixel height (after rotation, before scale)
    pub physical_height: u32,
    pub scale: f64,
    pub transform: u32,
    pub primary: bool,
    pub refresh_rate: f64,
}

impl Monitor {
    pub fn is_portrait(&self) -> bool {
        self.height > self.width
    }

    pub fn rotation_label(&self) -> &'static str {
        match self.transform {
            0 => "0°",
            1 => "90°",
            2 => "180°",
            3 => "270°",
            4 => "Flipped",
            5 => "90° Flipped",
            6 => "180° Flipped",
            7 => "270° Flipped",
            _ => "Unknown",
        }
    }

    /// Estimate diagonal inches from display name if possible (e.g. `27"` -> 27.0), else 24.0.
    pub fn guessed_diagonal_inches(&self) -> f64 {
        for token in self.display_name.split_whitespace() {
            if let Some(num) = token.strip_suffix('"') {
                if let Ok(val) = num.parse::<f64>() {
                    if (10.0..=80.0).contains(&val) {
                        return val;
                    }
                }
            }
        }
        // Fallback heuristic from resolution
        let max_dim = self.physical_width.max(self.physical_height);
        if max_dim >= 3840 {
            27.0
        } else if max_dim >= 2560 {
            27.0
        } else {
            24.0
        }
    }
}

#[derive(Debug, Clone)]
pub struct DesktopLayout {
    pub monitors: Vec<Monitor>,
    pub min_x: i32,
    pub min_y: i32,
    pub total_logical_width: u32,
    pub total_logical_height: u32,
    pub max_scale: f64,
    pub canvas_width: u32,
    pub canvas_height: u32,
}

impl DesktopLayout {
    pub fn from_monitors(mut monitors: Vec<Monitor>) -> Result<Self> {
        if monitors.is_empty() {
            anyhow::bail!("No active monitors detected");
        }
        // Sort left-to-right, then top-to-bottom for predictable display order
        monitors.sort_by_key(|m| (m.x, m.y, m.connector.clone()));

        let min_x = monitors.iter().map(|m| m.x).min().unwrap_or(0);
        let min_y = monitors.iter().map(|m| m.y).min().unwrap_or(0);
        let max_x = monitors
            .iter()
            .map(|m| m.x + m.width as i32)
            .max()
            .unwrap_or(1920);
        let max_y = monitors
            .iter()
            .map(|m| m.y + m.height as i32)
            .max()
            .unwrap_or(1080);

        let total_logical_width = (max_x - min_x).max(1) as u32;
        let total_logical_height = (max_y - min_y).max(1) as u32;
        let max_scale = monitors
            .iter()
            .map(|m| m.scale)
            .fold(1.0_f64, f64::max);

        let canvas_width = ((total_logical_width as f64) * max_scale).round() as u32;
        let canvas_height = ((total_logical_height as f64) * max_scale).round() as u32;

        Ok(Self {
            monitors,
            min_x,
            min_y,
            total_logical_width,
            total_logical_height,
            max_scale,
            canvas_width,
            canvas_height,
        })
    }

    /// Returns (x, y, width, height) in stitched canvas pixel coordinates for a monitor.
    pub fn monitor_canvas_rect(&self, m: &Monitor) -> (u32, u32, u32, u32) {
        let x = (((m.x - self.min_x) as f64) * self.max_scale).round() as u32;
        let y = (((m.y - self.min_y) as f64) * self.max_scale).round() as u32;
        let w = ((m.width as f64) * self.max_scale).round().max(1.0) as u32;
        let h = ((m.height as f64) * self.max_scale).round().max(1.0) as u32;
        (x, y, w, h)
    }
}

type MonitorId = (String, String, String, String);
type ModeInfo = (
    String,
    i32,
    i32,
    f64,
    f64,
    Vec<f64>,
    HashMap<String, OwnedValue>,
);
type PhysicalMonitorEntry = (MonitorId, Vec<ModeInfo>, HashMap<String, OwnedValue>);
type LogicalMonitorEntry = (
    i32,
    i32,
    f64,
    u32,
    bool,
    Vec<MonitorId>,
    HashMap<String, OwnedValue>,
);
type DisplayConfigState = (
    u32,
    Vec<PhysicalMonitorEntry>,
    Vec<LogicalMonitorEntry>,
    HashMap<String, OwnedValue>,
);

/// Detects active monitors via GNOME Mutter's D-Bus DisplayConfig interface.
pub fn detect_layout() -> Result<DesktopLayout> {
    let conn = Connection::session().context("Failed to connect to session D-Bus")?;
    let reply = conn
        .call_method(
            Some("org.gnome.Mutter.DisplayConfig"),
            "/org/gnome/Mutter/DisplayConfig",
            Some("org.gnome.Mutter.DisplayConfig"),
            "GetCurrentState",
            &(),
        )
        .context("Failed to query org.gnome.Mutter.DisplayConfig.GetCurrentState")?;

    let (_serial, phys_monitors, logical_monitors, _props): DisplayConfigState = reply
        .body()
        .deserialize()
        .context("Failed to deserialize Mutter DisplayConfig state")?;

    struct PhysInfo {
        display_name: String,
        vendor: String,
        product: String,
        width: u32,
        height: u32,
        refresh_rate: f64,
    }

    let mut phys_map: HashMap<String, PhysInfo> = HashMap::new();
    for ((connector, vendor, product, _serial), modes, props) in phys_monitors {
        let display_name = props
            .get("display-name")
            .and_then(|v| String::try_from(v.clone()).ok())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("{} {}", vendor, product).trim().to_string());

        let current_mode = modes
            .iter()
            .find(|m| {
                m.6.get("is-current")
                    .and_then(|v| bool::try_from(v.clone()).ok())
                    .unwrap_or(false)
            })
            .or_else(|| modes.first());

        if let Some(mode) = current_mode {
            phys_map.insert(
                connector,
                PhysInfo {
                    display_name,
                    vendor,
                    product,
                    width: mode.1.max(1) as u32,
                    height: mode.2.max(1) as u32,
                    refresh_rate: mode.3,
                },
            );
        }
    }

    let mut monitors = Vec::new();
    for (x, y, scale, transform, primary, attached, _props) in logical_monitors {
        // Use the first attached monitor in the logical monitor group
        if let Some((connector, vendor, product, _serial)) = attached.into_iter().next() {
            if let Some(info) = phys_map.get(&connector) {
                let rotated = transform % 2 == 1;
                let (phys_w, phys_h) = if rotated {
                    (info.height, info.width)
                } else {
                    (info.width, info.height)
                };
                let safe_scale = if scale > 0.0 { scale } else { 1.0 };
                let logical_w = ((phys_w as f64) / safe_scale).round().max(1.0) as u32;
                let logical_h = ((phys_h as f64) / safe_scale).round().max(1.0) as u32;

                monitors.push(Monitor {
                    connector,
                    display_name: info.display_name.clone(),
                    vendor: if info.vendor.is_empty() {
                        vendor
                    } else {
                        info.vendor.clone()
                    },
                    product: if info.product.is_empty() {
                        product
                    } else {
                        info.product.clone()
                    },
                    x,
                    y,
                    width: logical_w,
                    height: logical_h,
                    physical_width: phys_w,
                    physical_height: phys_h,
                    scale: safe_scale,
                    transform,
                    primary,
                    refresh_rate: info.refresh_rate,
                });
            }
        }
    }

    DesktopLayout::from_monitors(monitors)
}
