use crate::backend::{apply_profile, is_gnome_dark_mode};
use crate::compositor::{compute_physical_layout, monitor_summary};
use crate::config::{
    AppConfig, AppearanceMode, FitMode, MonitorPhysicalConfig, WallpaperMode,
};
use crate::monitors::{detect_layout, DesktopLayout};
use crate::slideshow::{advance_profile_slideshow, list_images_in_folder};
use eframe::egui;
use egui::{Color32, ColorImage, CornerRadius, Pos2, Rect, Stroke, StrokeKind, TextureHandle, Vec2};
use image::GenericImageView;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

fn shared_tokio_rt() -> Option<&'static tokio::runtime::Runtime> {
    static RT: OnceLock<Option<tokio::runtime::Runtime>> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .ok()
    })
    .as_ref()
}

fn pick_via_zenity(is_folder: bool) -> Option<Option<PathBuf>> {
    let mut cmd = Command::new("zenity");
    cmd.arg("--file-selection");
    if is_folder {
        cmd.arg("--directory")
            .arg("--title=Select Wallpaper Folder");
    } else {
        cmd.arg("--title=Select Wallpaper Image")
            .arg("--file-filter=Images | *.png *.jpg *.jpeg *.webp *.bmp *.gif *.PNG *.JPG *.JPEG *.WEBP");
    }
    let output = cmd.output().ok()?;
    if output.status.success() {
        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if s.is_empty() {
            Some(None)
        } else {
            Some(Some(PathBuf::from(s)))
        }
    } else {
        // User cancelled
        Some(None)
    }
}

struct CachedTexture {
    handle: TextureHandle,
    size: [usize; 2],
}

enum PickerTarget {
    MonitorImage {
        connector: String,
        appearance: AppearanceMode,
    },
    MonitorFolder {
        connector: String,
        appearance: AppearanceMode,
    },
    SpannedImage {
        appearance: AppearanceMode,
    },
    SpannedFolder {
        appearance: AppearanceMode,
    },
}

pub struct RustpaperApp {
    config: AppConfig,
    layout: DesktopLayout,
    selected_monitor: String,
    editing_appearance: AppearanceMode,
    new_profile_name: String,
    show_new_profile_input: bool,

    textures: HashMap<PathBuf, Option<CachedTexture>>,
    status_message: String,
    status_is_error: bool,

    apply_rx: Option<Receiver<Result<String, String>>>,
    picker_rx: Option<Receiver<(PickerTarget, Option<PathBuf>)>>,
    is_applying: bool,
    last_slideshow_tick: Instant,
}

impl RustpaperApp {
    pub fn new(mut config: AppConfig, layout: DesktopLayout) -> Self {
        config.ensure_monitors_initialized(&layout);
        let selected_monitor = layout
            .monitors
            .iter()
            .find(|m| m.primary)
            .or_else(|| layout.monitors.first())
            .map(|m| m.connector.clone())
            .unwrap_or_default();

        let editing_appearance = if config.active_profile().separate_dark_mode && is_gnome_dark_mode()
        {
            AppearanceMode::Dark
        } else {
            AppearanceMode::Light
        };

        Self {
            config,
            layout,
            selected_monitor,
            editing_appearance,
            new_profile_name: String::new(),
            show_new_profile_input: false,
            textures: HashMap::new(),
            status_message: "Ready".to_string(),
            status_is_error: false,
            apply_rx: None,
            picker_rx: None,
            is_applying: false,
            last_slideshow_tick: Instant::now(),
        }
    }

    fn open_picker(&mut self, target: PickerTarget) {
        if self.picker_rx.is_some() {
            return;
        }
        let is_folder = matches!(
            target,
            PickerTarget::MonitorFolder { .. } | PickerTarget::SpannedFolder { .. }
        );
        let (tx, rx) = mpsc::channel();
        self.picker_rx = Some(rx);
        thread::spawn(move || {
            let picked = if let Some(zenity_result) = pick_via_zenity(is_folder) {
                zenity_result
            } else if let Some(rt) = shared_tokio_rt() {
                rt.block_on(async move {
                    let handle = if is_folder {
                        rfd::AsyncFileDialog::new().pick_folder().await
                    } else {
                        rfd::AsyncFileDialog::new()
                            .add_filter("Images", &["png", "jpg", "jpeg", "webp", "bmp", "gif"])
                            .pick_file()
                            .await
                    };
                    handle.map(|h| h.path().to_path_buf())
                })
            } else {
                None
            };
            let _ = tx.send((target, picked));
        });
    }

    fn set_status(&mut self, msg: impl Into<String>, is_error: bool) {
        self.status_message = msg.into();
        self.status_is_error = is_error;
    }

    fn save_config(&mut self) {
        if let Err(e) = self.config.save() {
            self.set_status(format!("Failed to save config: {e}"), true);
        }
    }

    fn trigger_apply(&mut self) {
        if self.is_applying {
            return;
        }
        self.save_config();
        let layout = self.layout.clone();
        let config = self.config.clone();
        let profile = config.active_profile().clone();
        let (tx, rx) = mpsc::channel();
        self.apply_rx = Some(rx);
        self.is_applying = true;
        self.set_status("Stitching and applying wallpaper…", false);

        thread::spawn(move || {
            let res = apply_profile(&layout, &config, &profile)
                .map(|_| "Wallpaper applied to GNOME!".to_string())
                .map_err(|e| format!("Apply failed: {e:#}"));
            let _ = tx.send(res);
        });
    }

    fn get_or_load_texture<'a>(
        textures: &'a mut HashMap<PathBuf, Option<CachedTexture>>,
        ctx: &egui::Context,
        path: &Path,
    ) -> Option<&'a CachedTexture> {
        if !textures.contains_key(path) {
            let loaded = load_preview_texture(ctx, path);
            textures.insert(path.to_path_buf(), loaded);
        }
        textures.get(path).and_then(|o| o.as_ref())
    }

    fn current_appearance(&self) -> AppearanceMode {
        if self.config.active_profile().separate_dark_mode {
            self.editing_appearance
        } else {
            AppearanceMode::Light
        }
    }

    fn draw_top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top_bar")
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(24, 24, 30))
                    .inner_margin(egui::Margin::symmetric(14, 10)),
            )
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.heading(
                        egui::RichText::new("Rustpaper")
                            .strong()
                            .color(Color32::from_rgb(240, 242, 248)),
                    );
                    ui.separator();

                    // Mode toggle
                    let mode = self.config.active_profile().mode;
                    if ui
                        .selectable_label(mode == WallpaperMode::PerMonitor, "Per-Monitor")
                        .clicked()
                    {
                        self.config.active_profile_mut().mode = WallpaperMode::PerMonitor;
                        self.save_config();
                    }
                    if ui
                        .selectable_label(mode == WallpaperMode::Spanned, "Spanned (PPI + Bezel)")
                        .clicked()
                    {
                        self.config.active_profile_mut().mode = WallpaperMode::Spanned;
                        self.save_config();
                    }

                    ui.separator();

                    // Profile selector
                    ui.label("Profile:");
                    let profile_names: Vec<String> = self.config.profiles.keys().cloned().collect();
                    let mut selected_profile = self.config.active_profile.clone();
                    egui::ComboBox::from_id_salt("profile_selector")
                        .selected_text(&selected_profile)
                        .show_ui(ui, |ui| {
                            for name in &profile_names {
                                ui.selectable_value(&mut selected_profile, name.clone(), name);
                            }
                        });
                    if selected_profile != self.config.active_profile {
                        self.config.active_profile = selected_profile;
                        self.save_config();
                    }

                    if ui.button("+").on_hover_text("Create new profile").clicked() {
                        self.show_new_profile_input = !self.show_new_profile_input;
                        self.new_profile_name.clear();
                    }
                    if self.config.profiles.len() > 1
                        && ui
                            .button("🗑")
                            .on_hover_text("Delete current profile")
                            .clicked()
                    {
                        let current = self.config.active_profile.clone();
                        self.config.profiles.remove(&current);
                        self.config.active_profile =
                            self.config.profiles.keys().next().unwrap().clone();
                        self.save_config();
                    }

                    if self.show_new_profile_input {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.new_profile_name)
                                .desired_width(110.0)
                                .hint_text("Profile name"),
                        );
                        if ui.button("Save").clicked() {
                            let name = self.new_profile_name.trim().to_string();
                            if !name.is_empty() && !self.config.profiles.contains_key(&name) {
                                let cloned = self.config.active_profile().clone();
                                self.config.profiles.insert(name.clone(), cloned);
                                self.config.active_profile = name;
                                self.show_new_profile_input = false;
                                self.save_config();
                            }
                        }
                    }

                    ui.separator();

                    // Separate Light/Dark toggle
                    let mut sep_dark = self.config.active_profile().separate_dark_mode;
                    if ui.checkbox(&mut sep_dark, "Separate Dark Mode").changed() {
                        self.config.active_profile_mut().separate_dark_mode = sep_dark;
                        if !sep_dark {
                            self.editing_appearance = AppearanceMode::Light;
                        }
                        self.save_config();
                    }
                    if sep_dark {
                        ui.selectable_value(
                            &mut self.editing_appearance,
                            AppearanceMode::Light,
                            "☀ Light",
                        );
                        ui.selectable_value(
                            &mut self.editing_appearance,
                            AppearanceMode::Dark,
                            "🌙 Dark",
                        );
                    }

                    // Right-aligned action buttons
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let apply_btn = egui::Button::new(
                            egui::RichText::new(if self.is_applying {
                                "Applying…"
                            } else {
                                "Apply Wallpaper"
                            })
                            .strong()
                            .color(Color32::WHITE),
                        )
                        .fill(Color32::from_rgb(56, 118, 232));

                        if ui.add_enabled(!self.is_applying, apply_btn).clicked() {
                            self.trigger_apply();
                        }

                        if ui
                            .button("↻ Monitors")
                            .on_hover_text("Re-detect connected monitors from GNOME")
                            .clicked()
                        {
                            match detect_layout() {
                                Ok(new_layout) => {
                                    self.layout = new_layout;
                                    self.config.ensure_monitors_initialized(&self.layout);
                                    if !self
                                        .layout
                                        .monitors
                                        .iter()
                                        .any(|m| m.connector == self.selected_monitor)
                                    {
                                        self.selected_monitor = self
                                            .layout
                                            .monitors
                                            .first()
                                            .map(|m| m.connector.clone())
                                            .unwrap_or_default();
                                    }
                                    self.set_status(
                                        format!("Detected {} monitor(s)", self.layout.monitors.len()),
                                        false,
                                    );
                                }
                                Err(e) => {
                                    self.set_status(format!("Monitor detect failed: {e}"), true);
                                }
                            }
                        }
                    });
                });
            });
    }

    fn draw_inspector(&mut self, ctx: &egui::Context) {
        let appearance = self.current_appearance();
        let mode = self.config.active_profile().mode;

        egui::SidePanel::right("inspector")
            .default_width(370.0)
            .min_width(320.0)
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(28, 28, 35))
                    .inner_margin(egui::Margin::same(14)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let mut changed = false;

                    // Background fill color
                    ui.horizontal(|ui| {
                        ui.label("Canvas Background:");
                        let bg = &mut self
                            .config
                            .active_profile_mut()
                            .theme_set_mut(appearance)
                            .background_color;
                        if ui.color_edit_button_srgb(bg).changed() {
                            changed = true;
                        }
                    });
                    ui.add_space(6.0);

                    // Monitor selector tabs (also clickable directly on the canvas)
                    ui.horizontal_wrapped(|ui| {
                        for m in &self.layout.monitors {
                            let label = if m.primary {
                                format!("{} ★", m.connector)
                            } else {
                                m.connector.clone()
                            };
                            let selected = self.selected_monitor == m.connector;
                            if ui.selectable_label(selected, label).clicked() {
                                self.selected_monitor = m.connector.clone();
                            }
                        }
                    });
                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(6.0);

                    match mode {
                        WallpaperMode::PerMonitor => {
                            changed |= self.draw_per_monitor_inspector(ui, appearance);
                        }
                        WallpaperMode::Spanned => {
                            changed |= self.draw_spanned_inspector(ui, appearance);
                        }
                    }

                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(6.0);

                    // Slideshow controls
                    ui.heading("Slideshow");
                    ui.add_space(4.0);
                    let mut advance_now = false;
                    {
                        let ss = &mut self.config.active_profile_mut().slideshow;
                        if ui.checkbox(&mut ss.enabled, "Enable automatic rotation").changed() {
                            changed = true;
                            self.last_slideshow_tick = Instant::now();
                        }
                        ui.horizontal(|ui| {
                            ui.label("Interval (minutes):");
                            if ui
                                .add(egui::DragValue::new(&mut ss.interval_minutes).range(1..=1440))
                                .changed()
                            {
                                changed = true;
                            }
                        });
                        if ui.checkbox(&mut ss.shuffle, "Shuffle").changed() {
                            changed = true;
                        }
                        if ui.button("⏭ Next Wallpaper Now").clicked() {
                            advance_now = true;
                        }
                    }

                    if advance_now {
                        if advance_profile_slideshow(self.config.active_profile_mut()) {
                            self.last_slideshow_tick = Instant::now();
                            self.save_config();
                            self.trigger_apply();
                        } else {
                            self.set_status(
                                "Set a slideshow folder first to rotate wallpapers.",
                                true,
                            );
                        }
                    }

                    if changed {
                        self.save_config();
                    }
                });
            });
    }

    fn draw_per_monitor_inspector(
        &mut self,
        ui: &mut egui::Ui,
        appearance: AppearanceMode,
    ) -> bool {
        let mut changed = false;
        let selected_mon = self
            .layout
            .monitors
            .iter()
            .find(|m| m.connector == self.selected_monitor)
            .cloned();

        let Some(mon) = selected_mon else {
            ui.label("Select a monitor on the canvas.");
            return false;
        };

        ui.heading(format!("Monitor: {}", mon.connector));
        ui.label(
            egui::RichText::new(format!(
                "{}\n{}×{} @ {:.0}Hz · Scale {:.2}× · {}",
                mon.display_name,
                mon.width,
                mon.height,
                mon.refresh_rate,
                mon.scale,
                mon.rotation_label()
            ))
            .small()
            .color(Color32::from_rgb(165, 170, 185)),
        );
        ui.add_space(8.0);

        let mut pending_picker: Option<PickerTarget> = None;
        let picker_busy = self.picker_rx.is_some();

        let mw = self
            .config
            .active_profile_mut()
            .theme_set_mut(appearance)
            .per_monitor
            .entry(mon.connector.clone())
            .or_default();

        ui.horizontal(|ui| {
            if ui
                .add_enabled(!picker_busy, egui::Button::new("🖼 Pick Image…"))
                .clicked()
            {
                pending_picker = Some(PickerTarget::MonitorImage {
                    connector: mon.connector.clone(),
                    appearance,
                });
            }
            if mw.image_path.is_some() && ui.button("Clear").clicked() {
                mw.image_path = None;
                changed = true;
            }
        });

        if let Some(p) = &mw.image_path {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| p.display().to_string());
            ui.label(egui::RichText::new(name).small().monospace())
                .on_hover_text(p.display().to_string());
        } else {
            ui.label(
                egui::RichText::new("No wallpaper selected")
                    .italics()
                    .color(Color32::GRAY),
            );
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Fit Mode:");
            egui::ComboBox::from_id_salt("fit_mode_combo")
                .selected_text(mw.fit_mode.label())
                .show_ui(ui, |ui| {
                    for fm in FitMode::ALL {
                        if ui.selectable_value(&mut mw.fit_mode, fm, fm.label()).changed() {
                            changed = true;
                        }
                    }
                });
        });

        if mw.fit_mode == FitMode::Fill {
            ui.add_space(4.0);
            if ui
                .add(egui::Slider::new(&mut mw.offset_x, -1.0..=1.0).text("Crop X"))
                .changed()
            {
                changed = true;
            }
            if ui
                .add(egui::Slider::new(&mut mw.offset_y, -1.0..=1.0).text("Crop Y"))
                .changed()
            {
                changed = true;
            }
            if (mw.offset_x != 0.0 || mw.offset_y != 0.0) && ui.small_button("Reset Crop").clicked()
            {
                mw.offset_x = 0.0;
                mw.offset_y = 0.0;
                changed = true;
            }
        }

        ui.add_space(8.0);
        ui.label(egui::RichText::new("Monitor Slideshow Folder").strong());
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!picker_busy, egui::Button::new("📁 Pick Folder…"))
                .clicked()
            {
                pending_picker = Some(PickerTarget::MonitorFolder {
                    connector: mon.connector.clone(),
                    appearance,
                });
            }
            if mw.folder_path.is_some() && ui.button("Clear").clicked() {
                mw.folder_path = None;
                changed = true;
            }
        });
        if let Some(dir) = &mw.folder_path {
            let count = list_images_in_folder(dir).len();
            ui.label(
                egui::RichText::new(format!("{} ({} images)", dir.display(), count))
                    .small()
                    .color(Color32::from_rgb(160, 180, 210)),
            );
        }

        if let Some(target) = pending_picker {
            self.open_picker(target);
        }

        changed
    }

    fn draw_spanned_inspector(&mut self, ui: &mut egui::Ui, appearance: AppearanceMode) -> bool {
        let mut changed = false;

        ui.heading("Spanned Wallpaper");
        ui.add_space(4.0);

        let mut pending_picker: Option<PickerTarget> = None;
        let picker_busy = self.picker_rx.is_some();

        {
            let spanned = &mut self
                .config
                .active_profile_mut()
                .theme_set_mut(appearance)
                .spanned;

            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!picker_busy, egui::Button::new("🖼 Pick Spanned Image…"))
                    .clicked()
                {
                    pending_picker = Some(PickerTarget::SpannedImage { appearance });
                }
                if spanned.image_path.is_some() && ui.button("Clear").clicked() {
                    spanned.image_path = None;
                    changed = true;
                }
            });

            if let Some(p) = &spanned.image_path {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| p.display().to_string());
                ui.label(egui::RichText::new(name).small().monospace())
                    .on_hover_text(p.display().to_string());
            } else {
                ui.label(
                    egui::RichText::new("No spanned image selected")
                        .italics()
                        .color(Color32::GRAY),
                );
            }

            ui.add_space(6.0);
            if ui
                .checkbox(&mut spanned.ppi_correction, "PPI / Mixed-Resolution Correction")
                .changed()
            {
                changed = true;
            }
            if ui
                .checkbox(&mut spanned.bezel_correction, "Bezel Compensation (mm)")
                .changed()
            {
                changed = true;
            }

            ui.add_space(4.0);
            if ui
                .add(egui::Slider::new(&mut spanned.zoom, 1.0..=3.0).text("Zoom"))
                .changed()
            {
                changed = true;
            }
            if ui
                .add(egui::Slider::new(&mut spanned.pan_x, -1.0..=1.0).text("Pan X"))
                .changed()
            {
                changed = true;
            }
            if ui
                .add(egui::Slider::new(&mut spanned.pan_y, -1.0..=1.0).text("Pan Y"))
                .changed()
            {
                changed = true;
            }

            ui.add_space(6.0);
            ui.label(egui::RichText::new("Spanned Slideshow Folder").strong());
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!picker_busy, egui::Button::new("📁 Pick Folder…"))
                    .clicked()
                {
                    pending_picker = Some(PickerTarget::SpannedFolder { appearance });
                }
                if spanned.folder_path.is_some() && ui.button("Clear").clicked() {
                    spanned.folder_path = None;
                    changed = true;
                }
            });
            if let Some(dir) = &spanned.folder_path {
                let count = list_images_in_folder(dir).len();
                ui.label(
                    egui::RichText::new(format!("{} ({} images)", dir.display(), count))
                        .small()
                        .color(Color32::from_rgb(160, 180, 210)),
                );
            }
        }

        if let Some(target) = pending_picker {
            self.open_picker(target);
        }

        // Physical calibration for selected monitor
        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);
        ui.heading(format!("Physical Calibration ({})", self.selected_monitor));
        ui.label(
            egui::RichText::new("Click any monitor in the preview to calibrate its size & bezels.")
                .small()
                .color(Color32::from_rgb(155, 160, 175)),
        );
        ui.add_space(4.0);

        let phys = self
            .config
            .monitor_physical
            .entry(self.selected_monitor.clone())
            .or_insert_with(|| MonitorPhysicalConfig::default_for_diagonal(24.0));

        ui.horizontal(|ui| {
            ui.label("Diagonal (in):");
            if ui
                .add(
                    egui::DragValue::new(&mut phys.diagonal_inches)
                        .speed(0.1)
                        .range(10.0..=85.0)
                        .suffix("\""),
                )
                .changed()
            {
                changed = true;
            }
        });

        ui.label("Bezels (mm):");
        egui::Grid::new("bezel_grid").num_columns(4).show(ui, |ui| {
            ui.label("Left:");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut phys.bezel_mm.left)
                        .speed(0.5)
                        .range(0.0..=100.0),
                )
                .changed();
            ui.label("Right:");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut phys.bezel_mm.right)
                        .speed(0.5)
                        .range(0.0..=100.0),
                )
                .changed();
            ui.end_row();

            ui.label("Top:");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut phys.bezel_mm.top)
                        .speed(0.5)
                        .range(0.0..=100.0),
                )
                .changed();
            ui.label("Bottom:");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut phys.bezel_mm.bottom)
                        .speed(0.5)
                        .range(0.0..=100.0),
                )
                .changed();
            ui.end_row();
        });

        ui.label("Manual Alignment Nudge (mm):");
        ui.horizontal(|ui| {
            ui.label("X:");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut phys.offset_x_mm)
                        .speed(1.0)
                        .range(-300.0..=300.0),
                )
                .changed();
            ui.label("Y:");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut phys.offset_y_mm)
                        .speed(1.0)
                        .range(-300.0..=300.0),
                )
                .changed();
        });

        changed
    }

    fn draw_canvas(&mut self, ctx: &egui::Context) {
        let appearance = self.current_appearance();
        let profile = self.config.active_profile().clone();
        let theme_set = profile.theme_set(appearance).clone();
        let bg_rgb = theme_set.background_color;
        let bg_color = Color32::from_rgb(bg_rgb[0], bg_rgb[1], bg_rgb[2]);

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(16, 17, 22))
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let mode_badge = match profile.mode {
                        WallpaperMode::PerMonitor => "Per-Monitor Layout",
                        WallpaperMode::Spanned => "Physical Spanning Preview",
                    };
                    let theme_badge = if profile.separate_dark_mode {
                        match appearance {
                            AppearanceMode::Light => " · Light Mode",
                            AppearanceMode::Dark => " · Dark Mode",
                        }
                    } else {
                        ""
                    };
                    ui.label(
                        egui::RichText::new(format!(
                            "{}{}  ({}×{} canvas)",
                            mode_badge,
                            theme_badge,
                            self.layout.canvas_width,
                            self.layout.canvas_height
                        ))
                        .color(Color32::from_rgb(180, 185, 200)),
                    );
                });
                ui.add_space(8.0);

                let avail = ui.available_size();
                let (canvas_rect, _resp) =
                    ui.allocate_exact_size(avail, egui::Sense::empty());
                let painter = ui.painter_at(canvas_rect);

                // Subtle stage backdrop
                painter.rect_filled(
                    canvas_rect,
                    CornerRadius::same(10),
                    Color32::from_rgb(12, 13, 17),
                );
                painter.rect_stroke(
                    canvas_rect,
                    CornerRadius::same(10),
                    Stroke::new(1.0_f32, Color32::from_rgb(42, 45, 56)),
                    StrokeKind::Inside,
                );

                let pad = 28.0_f32;
                let inner = canvas_rect.shrink(pad);
                if inner.width() <= 10.0 || inner.height() <= 10.0 {
                    return;
                }

                match profile.mode {
                    WallpaperMode::PerMonitor => {
                        let scale = (inner.width() / self.layout.total_logical_width as f32)
                            .min(inner.height() / self.layout.total_logical_height as f32);
                        let draw_w = self.layout.total_logical_width as f32 * scale;
                        let draw_h = self.layout.total_logical_height as f32 * scale;
                        let origin = Pos2::new(
                            inner.center().x - draw_w * 0.5,
                            inner.center().y - draw_h * 0.5,
                        );

                        for m in &self.layout.monitors {
                            let mx = origin.x + (m.x - self.layout.min_x) as f32 * scale;
                            let my = origin.y + (m.y - self.layout.min_y) as f32 * scale;
                            let mw = m.width as f32 * scale;
                            let mh = m.height as f32 * scale;
                            let mrect = Rect::from_min_size(Pos2::new(mx, my), Vec2::new(mw, mh));

                            let click_resp = ui
                                .interact(
                                    mrect,
                                    ui.id().with(&m.connector),
                                    egui::Sense::click(),
                                )
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            if click_resp.clicked() {
                                self.selected_monitor = m.connector.clone();
                            }

                            painter.rect_filled(mrect, CornerRadius::same(4), bg_color);

                            if let Some(mcfg) = theme_set.per_monitor.get(&m.connector) {
                                if let Some(path) = &mcfg.image_path {
                                    if let Some(tex) =
                                        Self::get_or_load_texture(&mut self.textures, ctx, path)
                                    {
                                        paint_monitor_texture(
                                            &painter,
                                            mrect,
                                            m.physical_width,
                                            m.physical_height,
                                            tex,
                                            mcfg.fit_mode,
                                            mcfg.offset_x,
                                            mcfg.offset_y,
                                        );
                                    }
                                }
                            }

                            let is_selected = m.connector == self.selected_monitor;
                            let border_color = if is_selected {
                                Color32::from_rgb(86, 156, 255)
                            } else if click_resp.hovered() {
                                Color32::from_rgb(150, 165, 195)
                            } else {
                                Color32::from_rgb(68, 72, 88)
                            };
                            painter.rect_stroke(
                                mrect,
                                CornerRadius::same(4),
                                Stroke::new(if is_selected { 2.5_f32 } else { 1.2_f32 }, border_color),
                                StrokeKind::Inside,
                            );

                            paint_monitor_badge(&painter, mrect, &monitor_summary(m), is_selected);
                        }
                    }
                    WallpaperMode::Spanned => {
                        let spanned = &theme_set.spanned;
                        let phys = compute_physical_layout(
                            &self.layout,
                            &self.config.monitor_physical,
                            spanned.ppi_correction,
                            spanned.bezel_correction,
                        );
                        let scale = (inner.width() / phys.total_w as f32)
                            .min(inner.height() / phys.total_h as f32);
                        let draw_w = phys.total_w as f32 * scale;
                        let draw_h = phys.total_h as f32 * scale;
                        let origin = Pos2::new(
                            inner.center().x - draw_w * 0.5,
                            inner.center().y - draw_h * 0.5,
                        );

                        let tex_opt = spanned
                            .image_path
                            .as_ref()
                            .and_then(|p| Self::get_or_load_texture(&mut self.textures, ctx, p));

                        // Compute normalized UV window in the source image
                        let uv_window = tex_opt.map(|tex| {
                            let sw = tex.size[0] as f64;
                            let sh = tex.size[1] as f64;
                            let wall_aspect = phys.total_w / phys.total_h;
                            let src_aspect = sw / sh;
                            let zoom = spanned.zoom.clamp(1.0, 4.0) as f64;
                            let (vw, vh) = if src_aspect > wall_aspect {
                                let vh = 1.0 / zoom;
                                let vw = vh * (wall_aspect / src_aspect);
                                (vw, vh)
                            } else {
                                let vw = 1.0 / zoom;
                                let vh = vw * (src_aspect / wall_aspect);
                                (vw, vh)
                            };
                            let fx = ((spanned.pan_x.clamp(-1.0, 1.0) + 1.0) * 0.5) as f64;
                            let fy = ((spanned.pan_y.clamp(-1.0, 1.0) + 1.0) * 0.5) as f64;
                            let vx = (1.0 - vw).max(0.0) * fx;
                            let vy = (1.0 - vh).max(0.0) * fy;
                            (vx, vy, vw, vh)
                        });

                        for (m, prect) in self.layout.monitors.iter().zip(phys.rects.iter()) {
                            let mx = origin.x + prect.x_mm as f32 * scale;
                            let my = origin.y + prect.y_mm as f32 * scale;
                            let mw = prect.w_mm as f32 * scale;
                            let mh = prect.h_mm as f32 * scale;
                            let mrect = Rect::from_min_size(Pos2::new(mx, my), Vec2::new(mw, mh));

                            let click_resp = ui
                                .interact(
                                    mrect,
                                    ui.id().with("spanned").with(&m.connector),
                                    egui::Sense::click(),
                                )
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            if click_resp.clicked() {
                                self.selected_monitor = m.connector.clone();
                            }

                            painter.rect_filled(mrect, CornerRadius::same(4), bg_color);

                            if let (Some(tex), Some((vx, vy, vw, vh))) = (tex_opt, uv_window) {
                                let nx0 = prect.x_mm / phys.total_w;
                                let ny0 = prect.y_mm / phys.total_h;
                                let nw = prect.w_mm / phys.total_w;
                                let nh = prect.h_mm / phys.total_h;
                                let u0 = (vx + nx0 * vw) as f32;
                                let v0 = (vy + ny0 * vh) as f32;
                                let u1 = (vx + (nx0 + nw) * vw) as f32;
                                let v1 = (vy + (ny0 + nh) * vh) as f32;
                                let uv = Rect::from_min_max(Pos2::new(u0, v0), Pos2::new(u1, v1));
                                painter.image(tex.handle.id(), mrect, uv, Color32::WHITE);
                            }

                            let is_selected = m.connector == self.selected_monitor;
                            let border_color = if is_selected {
                                Color32::from_rgb(86, 156, 255)
                            } else if click_resp.hovered() {
                                Color32::from_rgb(150, 165, 195)
                            } else {
                                Color32::from_rgb(68, 72, 88)
                            };
                            painter.rect_stroke(
                                mrect,
                                CornerRadius::same(4),
                                Stroke::new(if is_selected { 2.5_f32 } else { 1.2_f32 }, border_color),
                                StrokeKind::Inside,
                            );

                            paint_monitor_badge(&painter, mrect, &monitor_summary(m), is_selected);
                        }
                    }
                }
            });
    }

    fn draw_status_bar(&self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status_bar")
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(22, 22, 28))
                    .inner_margin(egui::Margin::symmetric(14, 6)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let color = if self.status_is_error {
                        Color32::from_rgb(245, 115, 115)
                    } else {
                        Color32::from_rgb(160, 195, 165)
                    };
                    ui.label(egui::RichText::new(&self.status_message).small().color(color));
                });
            });
    }
}

impl eframe::App for RustpaperApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Handle drag-and-drop image files onto the window
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if let Some(path) = dropped
            .into_iter()
            .find(|p| crate::slideshow::is_supported_image(p))
        {
            let appearance = self.current_appearance();
            let mode = self.config.active_profile().mode;
            match mode {
                WallpaperMode::PerMonitor => {
                    let conn = self.selected_monitor.clone();
                    self.config
                        .active_profile_mut()
                        .theme_set_mut(appearance)
                        .per_monitor
                        .entry(conn)
                        .or_default()
                        .image_path = Some(path);
                }
                WallpaperMode::Spanned => {
                    self.config
                        .active_profile_mut()
                        .theme_set_mut(appearance)
                        .spanned
                        .image_path = Some(path);
                }
            }
            self.save_config();
        }

        // Poll async file/folder picker result
        if let Some(rx) = &self.picker_rx {
            match rx.try_recv() {
                Ok((target, picked)) => {
                    self.picker_rx = None;
                    if let Some(path) = picked {
                        match target {
                            PickerTarget::MonitorImage {
                                connector,
                                appearance,
                            } => {
                                self.config
                                    .active_profile_mut()
                                    .theme_set_mut(appearance)
                                    .per_monitor
                                    .entry(connector)
                                    .or_default()
                                    .image_path = Some(path);
                            }
                            PickerTarget::MonitorFolder {
                                connector,
                                appearance,
                            } => {
                                let imgs = list_images_in_folder(&path);
                                let mw = self
                                    .config
                                    .active_profile_mut()
                                    .theme_set_mut(appearance)
                                    .per_monitor
                                    .entry(connector)
                                    .or_default();
                                if mw.image_path.is_none() {
                                    mw.image_path = imgs.first().cloned();
                                }
                                mw.folder_path = Some(path);
                            }
                            PickerTarget::SpannedImage { appearance } => {
                                self.config
                                    .active_profile_mut()
                                    .theme_set_mut(appearance)
                                    .spanned
                                    .image_path = Some(path);
                            }
                            PickerTarget::SpannedFolder { appearance } => {
                                let imgs = list_images_in_folder(&path);
                                let sp = &mut self
                                    .config
                                    .active_profile_mut()
                                    .theme_set_mut(appearance)
                                    .spanned;
                                if sp.image_path.is_none() {
                                    sp.image_path = imgs.first().cloned();
                                }
                                sp.folder_path = Some(path);
                            }
                        }
                        self.save_config();
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(80));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.picker_rx = None;
                }
            }
        }

        // Poll async wallpaper apply result
        if let Some(rx) = &self.apply_rx {
            if let Ok(result) = rx.try_recv() {
                self.is_applying = false;
                self.apply_rx = None;
                match result {
                    Ok(msg) => self.set_status(msg, false),
                    Err(err) => self.set_status(err, true),
                }
            } else {
                ctx.request_repaint_after(Duration::from_millis(80));
            }
        }

        // Handle in-app slideshow timer
        let ss = self.config.active_profile().slideshow.clone();
        if ss.enabled && !self.is_applying {
            let interval = Duration::from_secs((ss.interval_minutes.max(1) as u64) * 60);
            if self.last_slideshow_tick.elapsed() >= interval {
                self.last_slideshow_tick = Instant::now();
                if advance_profile_slideshow(self.config.active_profile_mut()) {
                    self.trigger_apply();
                }
            }
            ctx.request_repaint_after(Duration::from_secs(5));
        }

        self.draw_top_bar(ctx);
        self.draw_status_bar(ctx);
        self.draw_inspector(ctx);
        self.draw_canvas(ctx);
    }
}

fn load_preview_texture(ctx: &egui::Context, path: &Path) -> Option<CachedTexture> {
    let img = image::open(path).ok()?;
    let (ow, oh) = img.dimensions();
    let resized = if ow > 1024 || oh > 1024 {
        img.thumbnail(1024, 1024)
    } else {
        img
    };
    let rgba = resized.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let color_image = ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
    let handle = ctx.load_texture(
        path.to_string_lossy(),
        color_image,
        egui::TextureOptions::LINEAR,
    );
    Some(CachedTexture {
        handle,
        size: [ow as usize, oh as usize],
    })
}

fn paint_monitor_texture(
    painter: &egui::Painter,
    mrect: Rect,
    mon_w: u32,
    mon_h: u32,
    tex: &CachedTexture,
    fit_mode: FitMode,
    offset_x: f32,
    offset_y: f32,
) {
    let sw = tex.size[0] as f32;
    let sh = tex.size[1] as f32;
    if sw <= 0.0 || sh <= 0.0 {
        return;
    }
    let src_aspect = sw / sh;
    let dst_aspect = mrect.width() / mrect.height().max(1.0);

    match fit_mode {
        FitMode::Fill => {
            let (u_span, v_span) = if src_aspect > dst_aspect {
                (dst_aspect / src_aspect, 1.0)
            } else {
                (1.0, src_aspect / dst_aspect)
            };
            let fx = (offset_x.clamp(-1.0, 1.0) + 1.0) * 0.5;
            let fy = (offset_y.clamp(-1.0, 1.0) + 1.0) * 0.5;
            let u0 = (1.0 - u_span) * fx;
            let v0 = (1.0 - v_span) * fy;
            let uv = Rect::from_min_max(Pos2::new(u0, v0), Pos2::new(u0 + u_span, v0 + v_span));
            painter.image(tex.handle.id(), mrect, uv, Color32::WHITE);
        }
        FitMode::Fit => {
            let (draw_w, draw_h) = if src_aspect > dst_aspect {
                (mrect.width(), mrect.width() / src_aspect)
            } else {
                (mrect.height() * src_aspect, mrect.height())
            };
            let target = Rect::from_center_size(mrect.center(), Vec2::new(draw_w, draw_h));
            let uv = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0));
            painter.image(tex.handle.id(), target, uv, Color32::WHITE);
        }
        FitMode::Stretch => {
            let uv = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0));
            painter.image(tex.handle.id(), mrect, uv, Color32::WHITE);
        }
        FitMode::Center => {
            let mw = mon_w.max(1) as f32;
            let mh = mon_h.max(1) as f32;
            let copy_w = sw.min(mw);
            let copy_h = sh.min(mh);

            let u_span = copy_w / sw;
            let v_span = copy_h / sh;
            let u0 = (1.0 - u_span) * 0.5;
            let v0 = (1.0 - v_span) * 0.5;
            let uv = Rect::from_min_max(Pos2::new(u0, v0), Pos2::new(u0 + u_span, v0 + v_span));

            let draw_w = mrect.width() * (copy_w / mw);
            let draw_h = mrect.height() * (copy_h / mh);
            let target = Rect::from_center_size(mrect.center(), Vec2::new(draw_w, draw_h));
            painter.image(tex.handle.id(), target, uv, Color32::WHITE);
        }
    }
}

fn paint_monitor_badge(painter: &egui::Painter, mrect: Rect, text: &str, selected: bool) {
    let pill_bg = if selected {
        Color32::from_rgba_unmultiplied(30, 64, 128, 225)
    } else {
        Color32::from_rgba_unmultiplied(15, 16, 22, 205)
    };
    let galley = painter.layout_no_wrap(
        text.to_string(),
        egui::FontId::proportional(14.5),
        Color32::from_rgb(240, 242, 248),
    );
    let max_w = (mrect.width() - 12.0).max(20.0);
    let badge_w = (galley.size().x + 16.0).min(max_w);
    let badge_h = galley.size().y + 8.0;
    let badge_rect = Rect::from_min_size(
        Pos2::new(mrect.left() + 6.0, mrect.bottom() - badge_h - 6.0),
        Vec2::new(badge_w, badge_h),
    );
    painter.rect_filled(badge_rect, CornerRadius::same(5), pill_bg);
    painter
        .with_clip_rect(badge_rect.shrink2(Vec2::new(5.0, 0.0)))
        .galley(
            Pos2::new(badge_rect.left() + 8.0, badge_rect.top() + 4.0),
            galley,
            Color32::WHITE,
        );
}

fn configure_typography(ctx: &egui::Context) {
    use egui::{FontFamily, FontId, TextStyle};

    let mut style = (*ctx.style()).clone();
    style.text_styles = [
        (TextStyle::Heading, FontId::new(21.0, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(15.5, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(15.5, FontFamily::Proportional)),
        (TextStyle::Monospace, FontId::new(14.5, FontFamily::Monospace)),
        (TextStyle::Small, FontId::new(13.5, FontFamily::Proportional)),
    ]
    .into();
    style.spacing.item_spacing = Vec2::new(8.0, 7.0);
    style.spacing.button_padding = Vec2::new(10.0, 5.0);
    style.spacing.interact_size.y = 26.0;
    ctx.set_style(style);
}

pub fn run_gui(config: AppConfig, layout: DesktopLayout) -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Rustpaper — Multi-Monitor Wallpaper Manager")
            .with_inner_size([1280.0, 780.0])
            .with_min_inner_size([920.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Rustpaper",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            configure_typography(&cc.egui_ctx);
            Ok(Box::new(RustpaperApp::new(config, layout)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))
}
