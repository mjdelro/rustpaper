mod backend;
mod compositor;
mod config;
mod monitors;
mod slideshow;
mod ui;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "rustpaper",
    version,
    about = "Fast Rust multi-monitor wallpaper manager for GNOME/Wayland"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// List detected monitors from GNOME Mutter DisplayConfig
    ListMonitors,
    /// Apply a saved profile (defaults to active profile)
    Apply {
        /// Optional profile name
        profile: Option<String>,
    },
    /// Advance slideshow folders to the next image and apply
    Next {
        /// Optional profile name
        profile: Option<String>,
    },
    /// Run a headless slideshow daemon using the profile's interval
    Daemon {
        /// Optional profile name
        profile: Option<String>,
    },
    /// Capture a PNG screenshot of the GUI window and exit
    #[command(hide = true)]
    Screenshot {
        /// Output PNG path
        path: PathBuf,
        /// Show Spanned mode in the screenshot
        #[arg(long)]
        spanned: bool,
    },
}

fn select_profile(cfg: &mut config::AppConfig, name: Option<String>) -> Result<()> {
    if let Some(p) = name {
        if !cfg.profiles.contains_key(&p) {
            anyhow::bail!(
                "Profile '{}' not found. Available: {}",
                p,
                cfg.profiles.keys().cloned().collect::<Vec<_>>().join(", ")
            );
        }
        cfg.active_profile = p;
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let layout = monitors::detect_layout()?;
    let mut cfg = config::AppConfig::load()?;
    cfg.ensure_monitors_initialized(&layout);

    match cli.command {
        None => {
            ui::run_gui(cfg, layout, None)?;
        }
        Some(Commands::Screenshot { path, spanned }) => {
            if spanned {
                let fallback_img = cfg
                    .active_profile()
                    .light
                    .per_monitor
                    .values()
                    .find_map(|m| m.image_path.clone());
                let prof = cfg.active_profile_mut();
                prof.mode = config::WallpaperMode::Spanned;
                if prof.light.spanned.image_path.is_none() {
                    prof.light.spanned.image_path = fallback_img;
                }
            }
            ui::run_gui(cfg, layout, Some(path))?;
        }
        Some(Commands::ListMonitors) => {
            println!(
                "Canvas: {}x{} (logical {}x{}, max scale {:.2}x)",
                layout.canvas_width,
                layout.canvas_height,
                layout.total_logical_width,
                layout.total_logical_height,
                layout.max_scale
            );
            for m in &layout.monitors {
                let (cx, cy, cw, ch) = layout.monitor_canvas_rect(m);
                println!(
                    "  {} [{}] — {} | pos=({},{}) logical={}x{} phys={}x{} scale={:.2} rot={} primary={} canvas=({},{},{}x{})",
                    m.connector,
                    m.product,
                    m.display_name,
                    m.x,
                    m.y,
                    m.width,
                    m.height,
                    m.physical_width,
                    m.physical_height,
                    m.scale,
                    m.rotation_label(),
                    m.primary,
                    cx,
                    cy,
                    cw,
                    ch
                );
            }
        }
        Some(Commands::Apply { profile }) => {
            select_profile(&mut cfg, profile)?;
            let prof = cfg.active_profile().clone();
            let (light, dark) = backend::apply_profile(&layout, &cfg, &prof)?;
            println!("Applied profile '{}'", cfg.active_profile);
            println!("  Light: {}", light.display());
            if prof.separate_dark_mode {
                println!("  Dark:  {}", dark.display());
            }
        }
        Some(Commands::Next { profile }) => {
            select_profile(&mut cfg, profile)?;
            let changed = slideshow::advance_profile_slideshow(cfg.active_profile_mut());
            if !changed {
                anyhow::bail!("No slideshow folders configured in profile '{}'", cfg.active_profile);
            }
            cfg.save()?;
            let prof = cfg.active_profile().clone();
            backend::apply_profile(&layout, &cfg, &prof)?;
            println!("Advanced slideshow and applied profile '{}'", cfg.active_profile);
        }
        Some(Commands::Daemon { profile }) => {
            select_profile(&mut cfg, profile)?;
            let interval_mins = cfg.active_profile().slideshow.interval_minutes.max(1);
            println!(
                "Starting Rustpaper slideshow daemon for profile '{}' (every {} min)…",
                cfg.active_profile, interval_mins
            );
            loop {
                let live_layout = monitors::detect_layout().unwrap_or_else(|_| layout.clone());
                let changed = slideshow::advance_profile_slideshow(cfg.active_profile_mut());
                if changed {
                    let _ = cfg.save();
                }
                let prof = cfg.active_profile().clone();
                if let Err(e) = backend::apply_profile(&live_layout, &cfg, &prof) {
                    eprintln!("Failed to apply wallpaper: {e:#}");
                }
                thread::sleep(Duration::from_secs(interval_mins as u64 * 60));
            }
        }
    }

    Ok(())
}
