//! `meetrec.exe --settings`: a small egui window that edits config.toml.
//! The tray process notices the file change and reloads; it never loads egui.

use std::path::PathBuf;

use anyhow::Result;
use eframe::egui;

use crate::audio::device::{self, DeviceInfo};
use crate::config::{CaptureMode, Config, KNOWN_APPS};

/// Known apps as checkboxes, everything else as comma-separated custom names.
pub fn split_trigger_apps(apps: &[String]) -> (Vec<(String, bool)>, String) {
    let known = KNOWN_APPS
        .iter()
        .map(|k| (k.to_string(), apps.iter().any(|a| a.eq_ignore_ascii_case(k))))
        .collect();
    let custom: Vec<&str> = apps
        .iter()
        .filter(|a| !KNOWN_APPS.iter().any(|k| a.eq_ignore_ascii_case(k)))
        .map(String::as_str)
        .collect();
    (known, custom.join(", "))
}

pub fn join_trigger_apps(known: &[(String, bool)], custom: &str) -> Vec<String> {
    known
        .iter()
        .filter(|(_, on)| *on)
        .map(|(name, _)| name.clone())
        .chain(custom.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from))
        .collect()
}

const WINDOW_SIZE: [f32; 2] = [420.0, 640.0];

pub fn run(config_path: PathBuf) -> Result<()> {
    let config = Config::load_or_create(&config_path);
    // Device enumeration needs an MTA thread; eframe's main thread is STA.
    let mics = std::thread::spawn(|| {
        let _ = wasapi::initialize_mta();
        device::list(wasapi::Direction::Capture).unwrap_or_default()
    })
    .join()
    .unwrap_or_default();
    let app = SettingsApp::new(config_path, config, mics);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("MeetRec settings")
            .with_inner_size(WINDOW_SIZE)
            .with_resizable(false),
        ..Default::default()
    };
    eframe::run_native("MeetRec settings", options, Box::new(|_cc| Ok(Box::new(app))))
        .map_err(|e| anyhow::anyhow!("settings window: {e}"))
}

struct SettingsApp {
    path: PathBuf,
    config: Config,
    mics: Vec<DeviceInfo>,
    known: Vec<(String, bool)>,
    custom: String,
    error: Option<String>,
}

impl SettingsApp {
    fn new(path: PathBuf, config: Config, mics: Vec<DeviceInfo>) -> Self {
        let (known, custom) = split_trigger_apps(&config.trigger_apps);
        Self { path, config, mics, known, custom, error: None }
    }

    fn save(&mut self) -> bool {
        self.config.trigger_apps = join_trigger_apps(&self.known, &self.custom);
        let config = self.config.clone().sanitized();
        match config.save(&self.path) {
            Ok(()) => true,
            Err(e) => {
                self.error = Some(format!("Could not save: {e}"));
                false
            }
        }
    }

    fn mic_label(&self) -> String {
        if self.config.mic_device.is_empty() {
            return "Windows default".into();
        }
        self.mics
            .iter()
            .find(|d| d.id == self.config.mic_device)
            .map(|d| d.name.clone())
            .unwrap_or_else(|| "(device not connected)".into())
    }
}

impl eframe::App for SettingsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.layout(ui);
    }
}

impl SettingsApp {
    /// Save/Cancel are pinned to the bottom; the form scrolls in the space
    /// above. Returns the button row's rect (for tests).
    fn layout(&mut self, ui: &mut egui::Ui) -> egui::Rect {
        let mut buttons = egui::Rect::NOTHING;
        egui::Panel::bottom("buttons").show(ui, |ui| {
            ui.add_space(4.0);
            if let Some(err) = &self.error {
                ui.colored_label(egui::Color32::RED, err);
            }
            buttons = ui
                .horizontal(|ui| {
                    if ui.button("Save").clicked() && self.save() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui.button("Cancel").clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                })
                .response
                .rect;
            ui.add_space(4.0);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.form(ui));
        });
        buttons
    }
}

impl SettingsApp {
    fn form(&mut self, ui: &mut egui::Ui) {
        let label = self.mic_label();
        let c = &mut self.config;

        ui.heading("Capture");
        ui.radio_value(&mut c.capture_mode, CaptureMode::Both, "Microphone + system audio (left = me, right = others)");
        ui.radio_value(&mut c.capture_mode, CaptureMode::Mic, "Microphone only");
        ui.radio_value(&mut c.capture_mode, CaptureMode::System, "System audio only");

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Microphone");
            egui::ComboBox::from_id_salt("mic").selected_text(label).width(280.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut c.mic_device, String::new(), "Windows default");
                for d in &self.mics {
                    ui.selectable_value(&mut c.mic_device, d.id.clone(), &d.name);
                }
            });
        });

        ui.add_space(10.0);
        ui.heading("Recordings");
        ui.horizontal(|ui| {
            let mut dir = c.output_dir.display().to_string();
            if ui.add(egui::TextEdit::singleline(&mut dir).desired_width(290.0)).changed() {
                c.output_dir = PathBuf::from(dir);
            }
            if ui.button("Browse…").clicked() {
                if let Some(picked) = rfd::FileDialog::new().set_directory(&c.output_dir).pick_folder() {
                    c.output_dir = picked;
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label("Bitrate per channel");
            ui.add(egui::DragValue::new(&mut c.bitrate_kbps).range(6..=128).suffix(" kbps"));
        });

        ui.add_space(10.0);
        ui.heading("Auto-record");
        ui.checkbox(&mut c.auto_record, "Record automatically when a meeting app uses the microphone");
        ui.add_enabled_ui(c.auto_record, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (name, on) in &mut self.known {
                    ui.checkbox(on, name.as_str());
                }
            });
            ui.horizontal(|ui| {
                ui.label("Other apps (exe names, comma-separated)");
            });
            ui.add(egui::TextEdit::singleline(&mut self.custom).hint_text("Discord, Signal").desired_width(380.0));
            ui.horizontal(|ui| {
                ui.label("Stop after the mic is released for");
                ui.add(egui::DragValue::new(&mut c.release_grace_secs).range(1..=600).suffix(" s"));
            });
            ui.horizontal(|ui| {
                ui.label("Discard auto recordings shorter than");
                ui.add(egui::DragValue::new(&mut c.min_duration_secs).range(0..=3600).suffix(" s"));
            });
        });

        ui.add_space(10.0);
        ui.heading("Hotkeys");
        egui::Grid::new("hotkeys").num_columns(2).show(ui, |ui| {
            for (label, value) in [
                ("Start / stop", &mut c.hotkeys.start_stop),
                ("Pause / resume", &mut c.hotkeys.pause),
                ("Split", &mut c.hotkeys.split),
                ("Add marker", &mut c.hotkeys.marker),
            ] {
                ui.label(label);
                ui.add(egui::TextEdit::singleline(value).hint_text("e.g. ctrl+alt+R").desired_width(200.0));
                ui.end_row();
            }
        });

        ui.add_space(10.0);
        ui.checkbox(&mut c.start_with_windows, "Start MeetRec when I sign in to Windows");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn known_apps_become_checkboxes_and_the_rest_custom_text() {
        let (known, custom) = split_trigger_apps(&s(&["zoom", "Teams", "Discord", "Signal"]));
        assert_eq!(known.len(), KNOWN_APPS.len());
        assert!(known.iter().find(|(n, _)| n == "Zoom").unwrap().1);
        assert!(known.iter().find(|(n, _)| n == "Teams").unwrap().1);
        assert!(!known.iter().find(|(n, _)| n == "Chrome").unwrap().1);
        assert_eq!(custom, "Discord, Signal");
    }

    #[test]
    fn joining_keeps_checked_known_apps_then_custom_names() {
        let known = vec![("Teams".to_string(), true), ("Zoom".to_string(), false), ("Chrome".to_string(), true)];
        assert_eq!(join_trigger_apps(&known, " Discord ,, Signal, "), s(&["Teams", "Chrome", "Discord", "Signal"]));
    }

    fn app_with(path: PathBuf, config: Config) -> SettingsApp {
        let mics = vec![DeviceInfo { id: "{mic-1}".into(), name: "Headset Mic".into() }];
        SettingsApp::new(path, config, mics)
    }

    #[test]
    fn form_lays_out_headlessly() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.mic_device = "{mic-1}".into();
        let mut app = app_with(dir.path().join("config.toml"), config);
        assert_eq!(app.mic_label(), "Headset Mic");
        let ctx = egui::Context::default();
        for _ in 0..2 {
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| app.form(ui));
            out.textures_delta.clear(); // no renderer to hand the font atlas to
        }
    }

    #[test]
    fn save_and_cancel_fit_in_the_default_window() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(dir.path().join("config.toml"), Config::default());
        app.error = Some("Could not save: disk full".into());
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::from(WINDOW_SIZE));
        let ctx = egui::Context::default();
        let mut buttons = egui::Rect::NOTHING;
        for _ in 0..2 {
            let input = egui::RawInput { screen_rect: Some(screen), ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| buttons = app.layout(ui));
            out.textures_delta.clear();
        }
        assert!(screen.contains_rect(buttons), "buttons {buttons:?} outside window {screen:?}");
    }

    #[test]
    fn save_writes_sanitized_config_with_merged_apps() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut app = app_with(path.clone(), Config::default());
        for (name, on) in &mut app.known {
            *on = name == "Zoom";
        }
        app.custom = "Discord".into();
        app.config.bitrate_kbps = 999;
        assert!(app.save());
        let saved = Config::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved.trigger_apps, s(&["Zoom", "Discord"]));
        assert_eq!(saved.bitrate_kbps, 128);
    }

    #[test]
    fn unknown_mic_id_is_labelled_disconnected() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.mic_device = "{gone}".into();
        assert_eq!(app_with(dir.path().join("c.toml"), config).mic_label(), "(device not connected)");
    }

    #[test]
    fn round_trip() {
        let apps = s(&["Teams", "Slack", "Discord"]);
        let (known, custom) = split_trigger_apps(&apps);
        assert_eq!(join_trigger_apps(&known, &custom), apps);
    }
}
