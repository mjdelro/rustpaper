use crate::monitors::DesktopLayout;
use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WallpaperMode {
    #[default]
    PerMonitor,
    Spanned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FitMode {
    Fill,
    Fit,
    Stretch,
    #[default]
    Center,
}

impl FitMode {
    pub const ALL: [FitMode; 4] = [FitMode::Fill, FitMode::Fit, FitMode::Stretch, FitMode::Center];

    pub fn label(self) -> &'static str {
        match self {
            FitMode::Fill => "Fill (Crop)",
            FitMode::Fit => "Fit (Letterbox)",
            FitMode::Stretch => "Stretch",
            FitMode::Center => "Center",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AppearanceMode {
    #[default]
    Light,
    Dark,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BezelMm {
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
}

impl Default for BezelMm {
    fn default() -> Self {
        Self {
            left: 8.0,
            right: 8.0,
            top: 8.0,
            bottom: 12.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorPhysicalConfig {
    pub diagonal_inches: f64,
    pub bezel_mm: BezelMm,
    /// Optional manual physical alignment adjustment in millimeters
    #[serde(default)]
    pub offset_x_mm: f64,
    #[serde(default)]
    pub offset_y_mm: f64,
}

impl MonitorPhysicalConfig {
    pub fn default_for_diagonal(diagonal_inches: f64) -> Self {
        Self {
            diagonal_inches,
            bezel_mm: BezelMm::default(),
            offset_x_mm: 0.0,
            offset_y_mm: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorWallpaper {
    pub image_path: Option<PathBuf>,
    pub folder_path: Option<PathBuf>,
    #[serde(default)]
    pub fit_mode: FitMode,
    /// Focal crop offset X (-1.0 = left edge, 0.0 = center, 1.0 = right edge)
    #[serde(default)]
    pub offset_x: f32,
    /// Focal crop offset Y (-1.0 = top edge, 0.0 = center, 1.0 = bottom edge)
    #[serde(default)]
    pub offset_y: f32,
}

impl Default for MonitorWallpaper {
    fn default() -> Self {
        Self {
            image_path: None,
            folder_path: None,
            fit_mode: FitMode::Center,
            offset_x: 0.0,
            offset_y: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpannedConfig {
    pub image_path: Option<PathBuf>,
    pub folder_path: Option<PathBuf>,
    pub ppi_correction: bool,
    pub bezel_correction: bool,
    pub zoom: f32,
    pub pan_x: f32,
    pub pan_y: f32,
}

impl Default for SpannedConfig {
    fn default() -> Self {
        Self {
            image_path: None,
            folder_path: None,
            ppi_correction: true,
            bezel_correction: true,
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeWallpaperSet {
    pub background_color: [u8; 3],
    pub per_monitor: BTreeMap<String, MonitorWallpaper>,
    pub spanned: SpannedConfig,
}

impl Default for ThemeWallpaperSet {
    fn default() -> Self {
        Self {
            background_color: [18, 18, 22],
            per_monitor: BTreeMap::new(),
            spanned: SpannedConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlideshowConfig {
    pub enabled: bool,
    pub interval_minutes: u32,
    pub shuffle: bool,
}

impl Default for SlideshowConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_minutes: 30,
            shuffle: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub mode: WallpaperMode,
    pub separate_dark_mode: bool,
    pub light: ThemeWallpaperSet,
    pub dark: ThemeWallpaperSet,
    pub slideshow: SlideshowConfig,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            mode: WallpaperMode::PerMonitor,
            separate_dark_mode: false,
            light: ThemeWallpaperSet::default(),
            dark: ThemeWallpaperSet::default(),
            slideshow: SlideshowConfig::default(),
        }
    }
}

impl Profile {
    pub fn theme_set(&self, appearance: AppearanceMode) -> &ThemeWallpaperSet {
        match appearance {
            AppearanceMode::Light => &self.light,
            AppearanceMode::Dark => &self.dark,
        }
    }

    pub fn theme_set_mut(&mut self, appearance: AppearanceMode) -> &mut ThemeWallpaperSet {
        match appearance {
            AppearanceMode::Light => &mut self.light,
            AppearanceMode::Dark => &mut self.dark,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub active_profile: String,
    pub profiles: BTreeMap<String, Profile>,
    pub monitor_physical: BTreeMap<String, MonitorPhysicalConfig>,
}

impl Default for AppConfig {
    fn default() -> Self {
        let mut profiles = BTreeMap::new();
        profiles.insert("Default".to_string(), Profile::default());
        Self {
            active_profile: "Default".to_string(),
            profiles,
            monitor_physical: BTreeMap::new(),
        }
    }
}

impl AppConfig {
    pub fn project_dirs() -> Result<ProjectDirs> {
        ProjectDirs::from("io", "rustpaper", "rustpaper")
            .context("Could not determine XDG config/cache directories")
    }

    pub fn config_path() -> Result<PathBuf> {
        let dirs = Self::project_dirs()?;
        Ok(dirs.config_dir().join("config.toml"))
    }

    pub fn cache_dir() -> Result<PathBuf> {
        let dirs = Self::project_dirs()?;
        let dir = dirs.cache_dir().to_path_buf();
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read config file {}", path.display()))?;
        let mut cfg: AppConfig = toml::from_str(&raw)
            .with_context(|| format!("Failed to parse config file {}", path.display()))?;
        if cfg.profiles.is_empty() {
            cfg.profiles.insert("Default".to_string(), Profile::default());
            cfg.active_profile = "Default".to_string();
        } else if !cfg.profiles.contains_key(&cfg.active_profile) {
            cfg.active_profile = cfg.profiles.keys().next().unwrap().clone();
        }
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let serialized = toml::to_string_pretty(self).context("Failed to serialize config")?;
        fs::write(&path, serialized)
            .with_context(|| format!("Failed to write config file {}", path.display()))?;
        Ok(())
    }

    pub fn ensure_monitors_initialized(&mut self, layout: &DesktopLayout) {
        for m in &layout.monitors {
            self.monitor_physical
                .entry(m.connector.clone())
                .or_insert_with(|| {
                    MonitorPhysicalConfig::default_for_diagonal(m.guessed_diagonal_inches())
                });
            for profile in self.profiles.values_mut() {
                profile
                    .light
                    .per_monitor
                    .entry(m.connector.clone())
                    .or_default();
                profile
                    .dark
                    .per_monitor
                    .entry(m.connector.clone())
                    .or_default();
            }
        }
    }

    pub fn active_profile(&self) -> &Profile {
        self.profiles
            .get(&self.active_profile)
            .unwrap_or_else(|| self.profiles.values().next().unwrap())
    }

    pub fn active_profile_mut(&mut self) -> &mut Profile {
        if !self.profiles.contains_key(&self.active_profile) {
            self.profiles
                .insert(self.active_profile.clone(), Profile::default());
        }
        self.profiles.get_mut(&self.active_profile).unwrap()
    }
}
