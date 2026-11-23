use crate::compositor::compose_and_save;
use crate::config::{AppConfig, Profile};
use crate::monitors::DesktopLayout;
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn file_uri(path: &Path) -> String {
    // Simple percent-encoding for spaces and special chars in file:// URIs
    let raw = path.to_string_lossy();
    let mut encoded = String::with_capacity(raw.len() + 16);
    encoded.push_str("file://");
    for b in raw.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(b as char);
            }
            _ => {
                encoded.push_str(&format!("%{:02X}", b));
            }
        }
    }
    encoded
}

fn gsettings_set(schema: &str, key: &str, value: &str) -> Result<()> {
    let status = Command::new("gsettings")
        .args(["set", schema, key, value])
        .status()
        .with_context(|| format!("Failed to invoke gsettings set {} {}", schema, key))?;
    if !status.success() {
        anyhow::bail!("gsettings set {} {} exited with {}", schema, key, status);
    }
    Ok(())
}

fn cleanup_old_cache(cache_dir: &Path, keep_light: &Path, keep_dark: &Path) {
    if let Ok(entries) = fs::read_dir(cache_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p == keep_light || p == keep_dark {
                continue;
            }
            if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                if name.starts_with("rustpaper-") && name.ends_with(".png") {
                    let _ = fs::remove_file(&p);
                }
            }
        }
    }
}

/// Composes and applies the active profile to GNOME (light + dark mode + spanned mode).
pub fn apply_profile(
    layout: &DesktopLayout,
    config: &AppConfig,
    profile: &Profile,
) -> Result<(PathBuf, PathBuf)> {
    let cache_dir = AppConfig::cache_dir()?;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);

    let light_path = cache_dir.join(format!("rustpaper-light-{}.png", ts));
    compose_and_save(
        layout,
        profile,
        &profile.light,
        &config.monitor_physical,
        &light_path,
    )?;

    let dark_path = if profile.separate_dark_mode {
        let dp = cache_dir.join(format!("rustpaper-dark-{}.png", ts));
        compose_and_save(
            layout,
            profile,
            &profile.dark,
            &config.monitor_physical,
            &dp,
        )?;
        dp
    } else {
        light_path.clone()
    };

    let light_uri = file_uri(&light_path);
    let dark_uri = file_uri(&dark_path);

    gsettings_set("org.gnome.desktop.background", "picture-options", "spanned")?;
    gsettings_set("org.gnome.desktop.background", "picture-uri", &light_uri)?;
    gsettings_set("org.gnome.desktop.background", "picture-uri-dark", &dark_uri)?;

    cleanup_old_cache(&cache_dir, &light_path, &dark_path);

    Ok((light_path, dark_path))
}

/// Detects whether GNOME is currently in dark mode (`prefer-dark`).
pub fn is_gnome_dark_mode() -> bool {
    Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.contains("prefer-dark"))
        .unwrap_or(true)
}
