use crate::config::{Profile, ThemeWallpaperSet, WallpaperMode};
use rand::Rng;
use std::fs;
use std::path::{Path, PathBuf};

const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp", "gif"];

pub fn is_supported_image(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|e| e.to_str())
            .map(|ext| IMAGE_EXTS.contains(&ext.to_ascii_lowercase().as_str()))
            .unwrap_or(false)
}

pub fn list_images_in_folder(folder: &Path) -> Vec<PathBuf> {
    let mut images = Vec::new();
    if let Ok(entries) = fs::read_dir(folder) {
        for entry in entries.flatten() {
            let path = entry.path();
            if is_supported_image(&path) {
                images.push(path);
            }
        }
    }
    images.sort();
    images
}

fn pick_next(images: &[PathBuf], current: Option<&PathBuf>, shuffle: bool) -> Option<PathBuf> {
    if images.is_empty() {
        return None;
    }
    if images.len() == 1 {
        return Some(images[0].clone());
    }
    if shuffle {
        let mut rng = rand::rng();
        for _ in 0..8 {
            let idx = rng.random_range(0..images.len());
            if current != Some(&images[idx]) {
                return Some(images[idx].clone());
            }
        }
        return Some(images[0].clone());
    }

    if let Some(curr) = current {
        if let Some(pos) = images.iter().position(|p| p == curr) {
            return Some(images[(pos + 1) % images.len()].clone());
        }
    }
    Some(images[0].clone())
}

fn advance_theme_set(
    mode: WallpaperMode,
    set: &mut ThemeWallpaperSet,
    shuffle: bool,
) -> bool {
    let mut changed = false;
    match mode {
        WallpaperMode::PerMonitor => {
            for mw in set.per_monitor.values_mut() {
                if let Some(folder) = &mw.folder_path {
                    let images = list_images_in_folder(folder);
                    if let Some(next) = pick_next(&images, mw.image_path.as_ref(), shuffle) {
                        mw.image_path = Some(next);
                        changed = true;
                    }
                }
            }
        }
        WallpaperMode::Spanned => {
            if let Some(folder) = &set.spanned.folder_path {
                let images = list_images_in_folder(folder);
                if let Some(next) = pick_next(&images, set.spanned.image_path.as_ref(), shuffle) {
                    set.spanned.image_path = Some(next);
                    changed = true;
                }
            }
        }
    }
    changed
}

/// Advances slideshow images for a profile. Returns `true` if at least one image changed.
pub fn advance_profile_slideshow(profile: &mut Profile) -> bool {
    let shuffle = profile.slideshow.shuffle;
    let mode = profile.mode;
    let mut changed = advance_theme_set(mode, &mut profile.light, shuffle);
    if profile.separate_dark_mode {
        changed |= advance_theme_set(mode, &mut profile.dark, shuffle);
    }
    changed
}
