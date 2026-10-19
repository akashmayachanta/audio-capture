//! The Recordings tab: lists finished recordings and plays the selected one.
//!
//! Playback is in-process (libopus + WASAPI), so stereo files play correctly
//! even where the system's Opus decoder only plays the first stream.

use std::path::PathBuf;
use std::time::Duration;

use eframe::egui;

use crate::playback::mix::Listen;
use crate::playback::player::{PlayState, Player, Status};
use crate::recordings::{self, format_time, Recording};

pub struct RecordingsTab {
    output_dir: PathBuf,
    items: Vec<Recording>,
    selected: Option<PathBuf>,
    /// Started on first use, so just opening the window touches no audio device.
    player: Option<Player>,
    listen: Listen,
    volume: f32,
    /// Slider position while the user drags it; the seek happens on release.
    scrub: Option<f64>,
}

impl RecordingsTab {
    pub fn new(output_dir: PathBuf) -> Self {
        let items = recordings::list(&output_dir);
        Self { output_dir, items, selected: None, player: None, listen: Listen::Both, volume: 1.0, scrub: None }
    }

    pub fn refresh(&mut self) {
        self.items = recordings::list(&self.output_dir);
    }

    fn player(&mut self) -> &Player {
        let (listen, volume) = (self.listen, self.volume);
        self.player.get_or_insert_with(|| {
            let p = Player::spawn();
            p.set_listen(listen);
            p.set_volume(volume);
            p
        })
    }

    fn select(&mut self, path: PathBuf, play: bool) {
        if self.selected.as_ref() != Some(&path) {
            self.selected = Some(path.clone());
            self.scrub = None;
            self.player().load(path);
        }
        if play {
            self.player().play();
        }
    }

    /// The list of recordings, newest first. Click loads, double-click plays.
    pub fn list(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("Refresh").clicked() {
                self.refresh();
            }
            if ui.button("Open folder").clicked() {
                let _ = std::fs::create_dir_all(&self.output_dir);
                if let Err(e) = std::process::Command::new("explorer").arg(&self.output_dir).spawn() {
                    tracing::warn!("open folder: {e}");
                }
            }
            ui.label(format!("{} recordings", self.items.len()));
        });
        ui.add_space(4.0);
        if self.items.is_empty() {
            ui.label(format!("No recordings yet in {}", self.output_dir.display()));
            return;
        }
        let mut clicked = None;
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for item in &self.items {
                let on = self.selected.as_ref() == Some(&item.path);
                let r = ui.selectable_label(on, item.label());
                if r.double_clicked() {
                    clicked = Some((item.path.clone(), true));
                } else if r.clicked() {
                    clicked = Some((item.path.clone(), false));
                }
            }
        });
        if let Some((path, play)) = clicked {
            self.select(path, play);
        }
    }

    /// Transport, seek bar, listen mode and volume. Returns their rect (for tests).
    pub fn controls(&mut self, ui: &mut egui::Ui) -> egui::Rect {
        let status = self.player.as_ref().map(Player::status).unwrap_or_default();
        if status.state == PlayState::Playing {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        let loaded = status.duration_secs > 0.0;
        ui.vertical(|ui| {
            ui.add_space(4.0);
            ui.label(now_playing(&status));
            if let Some(err) = &status.error {
                ui.colored_label(egui::Color32::RED, err);
            }
            ui.horizontal(|ui| self.transport(ui, &status, loaded));
            ui.horizontal(|ui| self.mix(ui, &status, loaded));
            ui.add_space(4.0);
        })
        .response
        .rect
    }

    fn transport(&mut self, ui: &mut egui::Ui, status: &Status, loaded: bool) {
        let playing = status.state == PlayState::Playing;
        if ui.add_enabled(loaded, egui::Button::new(if playing { "Pause" } else { "Play" })).clicked() {
            if playing { self.player().pause() } else { self.player().play() }
        }
        if ui.add_enabled(loaded && status.state != PlayState::Stopped, egui::Button::new("Stop")).clicked() {
            self.player().stop();
        }
        let dur = status.duration_secs;
        let mut pos = self.scrub.unwrap_or(status.position_secs).clamp(0.0, dur);
        ui.spacing_mut().slider_width = 190.0;
        let r = ui.add_enabled(loaded, egui::Slider::new(&mut pos, 0.0..=dur.max(0.001)).show_value(false));
        if r.dragged() {
            self.scrub = Some(pos);
        }
        if r.drag_stopped() || (r.changed() && !r.dragged()) {
            self.scrub = None;
            self.player().seek(pos);
        }
        ui.label(format!("{} / {}", format_time(pos), format_time(dur)));
    }

    fn mix(&mut self, ui: &mut egui::Ui, status: &Status, loaded: bool) {
        let before = self.listen;
        ui.add_enabled_ui(loaded && !status.mono, |ui| {
            ui.radio_value(&mut self.listen, Listen::Both, "Both");
            ui.radio_value(&mut self.listen, Listen::Me, "Me only");
            ui.radio_value(&mut self.listen, Listen::Others, "Others only");
        });
        if self.listen != before {
            let listen = self.listen;
            self.player().set_listen(listen);
        }
        ui.spacing_mut().slider_width = 80.0;
        let volume = egui::Slider::new(&mut self.volume, 0.0..=2.0)
            .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
            .text("Vol");
        if ui.add(volume).changed() {
            let volume = self.volume;
            self.player().set_volume(volume);
        }
    }
}

fn now_playing(status: &Status) -> String {
    match &status.file {
        Some(f) => f.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        None => "Select a recording (double-click plays it)".into(),
    }
}
