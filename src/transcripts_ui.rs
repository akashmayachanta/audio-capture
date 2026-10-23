//! `meetrec.exe --transcripts`: lists recordings with a Transcribe button per
//! row. A job is a `meetrec.exe --transcribe` child process, so it keeps going
//! if the window closes; its progress comes from `progress.json`, and a row is
//! "Transcribed" when `<name>.md` exists, so everything survives a reopen.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use anyhow::Result;
use eframe::egui;

use crate::config::Config;
use crate::recordings::{self, Recording};
use crate::transcribe::job::Device;
use crate::transcribe::paths::{format_size, CacheDirs};
use crate::transcribe::progress::{self, Phase};
use crate::transcribe::transcript::md_path;

const WINDOW_SIZE: [f32; 2] = [640.0, 520.0];
const POLL_EVERY: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, PartialEq)]
pub enum RowStatus {
    NotTranscribed,
    Queued,
    Transcribing(f32),
    Transcribed,
    Failed(String),
}

impl RowStatus {
    pub fn text(&self) -> String {
        match self {
            RowStatus::NotTranscribed => "Not transcribed".into(),
            RowStatus::Queued => "Queued".into(),
            RowStatus::Transcribing(p) => format!("Transcribing {:.0}%", p * 100.0),
            RowStatus::Transcribed => "Transcribed".into(),
            RowStatus::Failed(why) => format!("Failed ({why})"),
        }
    }
}

/// Starts `meetrec --transcribe` for (recording, job id, replace existing).
pub type Spawner = Box<dyn Fn(&Path, &str, bool, Device) -> std::io::Result<Child>>;

fn real_spawner() -> Spawner {
    Box::new(|recording, job_id, force, device| {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut cmd = Command::new(std::env::current_exe()?);
        cmd.arg("--transcribe").arg(recording).args(["--job-id", job_id, "--device", device.as_arg()]);
        if force {
            cmd.arg("--force");
        }
        cmd.creation_flags(CREATE_NO_WINDOW).spawn()
    })
}

struct Running {
    recording: PathBuf,
    job_id: String,
    child: Child,
    progress: f32,
}

/// A job some earlier window started.
struct External {
    job_id: String,
    pid: u32,
    progress: f32,
}

pub struct TranscriptsApp {
    config: Config,
    cache: CacheDirs,
    items: Vec<Recording>,
    device: Device,
    queue: VecDeque<(PathBuf, bool)>,
    running: Option<Running>,
    external: HashMap<PathBuf, External>,
    failures: HashMap<PathBuf, String>,
    /// Re-transcribe waiting for the user to confirm the overwrite.
    confirm: Option<PathBuf>,
    cache_bytes: u64,
    spawner: Spawner,
    last_poll: Instant,
}

pub fn run(config_path: PathBuf) -> Result<()> {
    let config = Config::load_or_create(&config_path);
    let app = TranscriptsApp::new(config, real_spawner());
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("MeetRec Transcripts")
            .with_inner_size(WINDOW_SIZE)
            .with_min_inner_size([520.0, 320.0]),
        ..Default::default()
    };
    eframe::run_native("MeetRec Transcripts", options, Box::new(|_cc| Ok(Box::new(app))))
        .map_err(|e| anyhow::anyhow!("transcripts window: {e}"))
}

impl TranscriptsApp {
    pub fn new(config: Config, spawner: Spawner) -> Self {
        let cache = CacheDirs::from_config(&config.transcription.cache_dir);
        Self::with_cache(config, cache, spawner)
    }

    pub fn with_cache(config: Config, cache: CacheDirs, spawner: Spawner) -> Self {
        let device = Device::parse(&config.transcription.device);
        let mut app = Self {
            config,
            cache,
            items: Vec::new(),
            device,
            queue: VecDeque::new(),
            running: None,
            external: HashMap::new(),
            failures: HashMap::new(),
            confirm: None,
            cache_bytes: 0,
            spawner,
            last_poll: Instant::now(),
        };
        app.refresh();
        app
    }

    /// Re-reads the recordings folder and the job records on disk.
    pub fn refresh(&mut self) {
        self.items = recordings::list(&self.config.output_dir);
        self.cache_bytes = self.cache.sizes().total();
        self.external.clear();
        for (job_id, state) in progress::scan(&self.cache) {
            let ours = self.running.as_ref().is_some_and(|r| r.job_id == job_id);
            match state.phase {
                Phase::Running if !ours => {
                    self.external.insert(
                        state.recording.clone(),
                        External { job_id, pid: state.pid, progress: state.progress },
                    );
                }
                Phase::Failed if !md_path(&state.recording).is_file() => {
                    self.failures.insert(state.recording, state.message);
                }
                _ => {}
            }
        }
    }

    pub fn status(&self, rec: &Recording) -> RowStatus {
        if let Some(r) = self.running.as_ref().filter(|r| r.recording == rec.path) {
            return RowStatus::Transcribing(r.progress);
        }
        if let Some(e) = self.external.get(&rec.path) {
            return RowStatus::Transcribing(e.progress);
        }
        if self.queue.iter().any(|(p, _)| *p == rec.path) {
            return RowStatus::Queued;
        }
        if rec.transcribed {
            return RowStatus::Transcribed;
        }
        match self.failures.get(&rec.path) {
            Some(why) => RowStatus::Failed(why.clone()),
            None => RowStatus::NotTranscribed,
        }
    }

    /// One job at a time, across windows too.
    pub fn busy(&self) -> bool {
        self.running.is_some() || !self.external.is_empty()
    }

    pub fn enqueue(&mut self, recording: PathBuf, force: bool) {
        let known = self.running.as_ref().is_some_and(|r| r.recording == recording)
            || self.external.contains_key(&recording)
            || self.queue.iter().any(|(p, _)| *p == recording);
        if !known {
            self.failures.remove(&recording);
            self.queue.push_back((recording, force));
        }
        self.pump();
    }

    /// Starts the next queued job when nothing is running.
    fn pump(&mut self) {
        while !self.busy() {
            let Some((recording, force)) = self.queue.pop_front() else { return };
            let job_id = uuid::Uuid::new_v4().simple().to_string();
            // A failed attempt's record would hide behind the new job's.
            for (id, state) in progress::scan(&self.cache) {
                if state.recording == recording {
                    let _ = std::fs::remove_dir_all(self.cache.tmp_job_dir(&id));
                }
            }
            match (self.spawner)(&recording, &job_id, force, self.device) {
                Ok(child) => self.running = Some(Running { recording, job_id, child, progress: 0.0 }),
                Err(e) => {
                    self.failures.insert(recording, format!("could not start: {e}"));
                }
            }
        }
    }

    /// Checks on the running job and on jobs from other windows. Call often.
    pub fn poll(&mut self) {
        let mut finished = false;
        if let Some(r) = &mut self.running {
            if let Some(state) = progress::read(&progress::state_path(&self.cache, &r.job_id)) {
                if state.phase == Phase::Running {
                    r.progress = state.progress;
                }
            }
            if let Ok(Some(status)) = r.child.try_wait() {
                let r = self.running.take().expect("checked above");
                let state = progress::read(&progress::state_path(&self.cache, &r.job_id));
                if status.success() && md_path(&r.recording).is_file() {
                    self.failures.remove(&r.recording);
                    let _ = std::fs::remove_dir_all(self.cache.tmp_job_dir(&r.job_id));
                } else {
                    let why = match state {
                        Some(s) if s.phase == Phase::Failed && !s.message.is_empty() => s.message,
                        _ => format!("the transcription process exited with {status}"),
                    };
                    self.failures.insert(r.recording, why);
                }
                finished = true;
            }
        }
        let had_external = self.external.len();
        if finished || had_external > 0 {
            self.refresh();
        }
        if finished || (had_external > 0 && self.external.is_empty()) {
            self.pump();
        }
    }

    /// Stops a running job (ours or another window's) and removes its scratch folder.
    pub fn cancel(&mut self, recording: &Path) {
        if self.running.as_ref().is_some_and(|r| r.recording == recording) {
            let mut r = self.running.take().expect("checked above");
            progress::kill_tree(r.child.id());
            let _ = r.child.wait();
            let _ = std::fs::remove_dir_all(self.cache.tmp_job_dir(&r.job_id));
        } else if let Some(e) = self.external.remove(recording) {
            progress::kill_tree(e.pid);
            let _ = std::fs::remove_dir_all(self.cache.tmp_job_dir(&e.job_id));
        } else {
            self.queue.retain(|(p, _)| p != recording);
        }
        self.pump();
    }
}

fn explorer(args: &[&std::ffi::OsStr]) {
    if let Err(e) = Command::new("explorer").args(args).spawn() {
        tracing::warn!("explorer: {e}");
    }
}

impl eframe::App for TranscriptsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.last_poll.elapsed() >= POLL_EVERY {
            self.last_poll = Instant::now();
            self.poll();
        }
        if self.busy() {
            ui.ctx().request_repaint_after(POLL_EVERY);
        }
        self.layout(ui);
    }
}

impl TranscriptsApp {
    /// Top bar, optional confirm bar, then the rows. Returns the top bar's rect (for tests).
    fn layout(&mut self, ui: &mut egui::Ui) -> egui::Rect {
        let mut bar = egui::Rect::NOTHING;
        egui::Panel::top("bar").show(ui, |ui| bar = self.top_bar(ui));
        if let Some(path) = self.confirm.clone() {
            egui::Panel::bottom("confirm").show(ui, |ui| {
                ui.add_space(4.0);
                ui.label(format!(
                    "Replace the transcript {}?",
                    md_path(&path).file_name().unwrap_or_default().to_string_lossy()
                ));
                ui.horizontal(|ui| {
                    if ui.button("Replace").clicked() {
                        self.enqueue(path.clone(), true);
                        self.confirm = None;
                    }
                    if ui.button("Keep it").clicked() {
                        self.confirm = None;
                    }
                });
                ui.add_space(4.0);
            });
        }
        egui::CentralPanel::default().show(ui, |ui| self.rows(ui));
        bar
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) -> egui::Rect {
        ui.add_space(4.0);
        let rect = ui
            .horizontal_wrapped(|ui| {
                if ui.button("Refresh").clicked() {
                    self.refresh();
                }
                if ui.button("Open recordings folder").clicked() {
                    let _ = std::fs::create_dir_all(&self.config.output_dir);
                    explorer(&[self.config.output_dir.as_os_str()]);
                }
                ui.label("Device");
                egui::ComboBox::from_id_salt("device").selected_text(self.device.label()).show_ui(ui, |ui| {
                    for d in Device::ALL {
                        ui.selectable_value(&mut self.device, d, d.label());
                    }
                });
                ui.label(format!("Cache {}", format_size(self.cache_bytes)))
                    .on_hover_text(self.cache.root().display().to_string());
            })
            .response
            .rect;
        if !self.config.transcription.enabled {
            ui.colored_label(egui::Color32::YELLOW, "Transcription is turned off in Settings.");
        }
        ui.add_space(4.0);
        rect
    }

    fn rows(&mut self, ui: &mut egui::Ui) {
        if self.items.is_empty() {
            ui.label(format!("No recordings yet in {}", self.config.output_dir.display()));
            return;
        }
        let mut action = None;
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for item in &self.items {
                let status = self.status(item);
                ui.horizontal(|ui| {
                    // Right-to-left, so the buttons claim their space first and the label takes the rest.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(a) = row_buttons(ui, item, &status, self.config.transcription.enabled) {
                            action = Some(a);
                        }
                        show_status(ui, &status);
                        ui.add(egui::Label::new(item.summary()).truncate());
                    });
                });
                ui.separator();
            }
        });
        match action {
            Some(Action::Transcribe(p)) => self.enqueue(p, false),
            Some(Action::Retranscribe(p)) => self.confirm = Some(p),
            Some(Action::Cancel(p)) => self.cancel(&p),
            Some(Action::OpenMd(p)) => explorer(&[md_path(&p).as_os_str()]),
            Some(Action::Show(p)) => {
                let target = if md_path(&p).is_file() { md_path(&p) } else { p };
                let mut arg = std::ffi::OsString::from("/select,");
                arg.push(target);
                explorer(&[arg.as_os_str()]);
            }
            None => {}
        }
    }
}

enum Action {
    Transcribe(PathBuf),
    Retranscribe(PathBuf),
    Cancel(PathBuf),
    OpenMd(PathBuf),
    Show(PathBuf),
}

/// The status cell: a progress bar while running, text otherwise (hover a failure for its reason).
fn show_status(ui: &mut egui::Ui, status: &RowStatus) {
    let size = [130.0, 18.0];
    match status {
        RowStatus::Transcribing(p) => {
            ui.add_sized(size, egui::ProgressBar::new(*p).text(status.text()));
        }
        RowStatus::Failed(why) => {
            let label = egui::Label::new(egui::RichText::new("Failed").color(egui::Color32::LIGHT_RED));
            ui.add_sized(size, label).on_hover_text(why);
        }
        _ => {
            ui.add_sized(size, egui::Label::new(status.text()));
        }
    }
}

fn row_buttons(ui: &mut egui::Ui, item: &Recording, status: &RowStatus, enabled: bool) -> Option<Action> {
    let path = || item.path.clone();
    let mut action = None;
    match status {
        RowStatus::NotTranscribed | RowStatus::Failed(_) => {
            if ui.add_enabled(enabled, egui::Button::new("Transcribe")).clicked() {
                action = Some(Action::Transcribe(path()));
            }
        }
        RowStatus::Queued => {
            if ui.button("Remove").clicked() {
                action = Some(Action::Cancel(path()));
            }
        }
        RowStatus::Transcribing(_) => {
            if ui.button("Cancel").clicked() {
                action = Some(Action::Cancel(path()));
            }
        }
        RowStatus::Transcribed => {
            if ui.add_enabled(enabled, egui::Button::new("Re-transcribe")).clicked() {
                action = Some(Action::Retranscribe(path()));
            }
            if ui.button("Show in folder").clicked() {
                action = Some(Action::Show(path()));
            }
            if ui.button("Open .md").clicked() {
                action = Some(Action::OpenMd(path()));
            }
        }
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcribe::progress::{state_path, JobState};
    use std::cell::RefCell;
    use std::rc::Rc;

    struct Fixture {
        dir: tempfile::TempDir,
        spawned: Rc<RefCell<Vec<String>>>,
    }

    impl Fixture {
        fn new() -> Self {
            Self { dir: tempfile::tempdir().unwrap(), spawned: Rc::default() }
        }

        fn cache(&self) -> CacheDirs {
            CacheDirs::new(self.dir.path().join("cache"))
        }

        fn recording(&self, name: &str) -> PathBuf {
            let path = self.dir.path().join("recordings").join(format!("{name}.opus"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"OggS").unwrap();
            path
        }

        /// `exit_code` is what the fake child exits with; `md` makes it leave a
        /// transcript behind; `failure` makes it record a failure like the real helper.
        fn spawner(&self, exit_code: i32, md: bool, failure: Option<&'static str>) -> Spawner {
            let spawned = self.spawned.clone();
            let cache = self.cache();
            Box::new(move |rec, job_id, _force, _device| {
                spawned.borrow_mut().push(rec.file_stem().unwrap().to_string_lossy().into_owned());
                if md {
                    std::fs::write(md_path(rec), "# Transcript").unwrap();
                }
                if let Some(message) = failure {
                    let state = JobState {
                        recording: rec.to_path_buf(),
                        pid: 0,
                        phase: Phase::Failed,
                        progress: 0.3,
                        message: message.into(),
                    };
                    progress::write(&state_path(&cache, job_id), &state).unwrap();
                }
                Command::new("cmd").args(["/C", &format!("exit {exit_code}")]).spawn()
            })
        }

        fn app(&self, spawner: Spawner) -> TranscriptsApp {
            let mut config = Config::default();
            config.output_dir = self.dir.path().join("recordings");
            TranscriptsApp::with_cache(config, self.cache(), spawner)
        }

        fn spawned(&self) -> Vec<String> {
            self.spawned.borrow().clone()
        }
    }

    /// Polls until the running job has been reaped.
    fn settle(app: &mut TranscriptsApp) {
        for _ in 0..200 {
            app.poll();
            if app.running.is_none() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("job never finished");
    }

    fn status_of(app: &TranscriptsApp, path: &Path) -> RowStatus {
        app.status(app.items.iter().find(|r| r.path == path).expect("listed"))
    }

    #[test]
    fn a_recording_is_transcribed_exactly_when_its_md_exists() {
        let fx = Fixture::new();
        let a = fx.recording("a");
        let b = fx.recording("b");
        std::fs::write(md_path(&b), "# Transcript").unwrap();
        let app = fx.app(fx.spawner(0, false, None));
        assert_eq!(status_of(&app, &a), RowStatus::NotTranscribed);
        assert_eq!(status_of(&app, &b), RowStatus::Transcribed);
    }

    #[test]
    fn jobs_run_one_at_a_time_in_order() {
        let fx = Fixture::new();
        let (a, b) = (fx.recording("a"), fx.recording("b"));
        let mut app = fx.app(fx.spawner(0, true, None));
        app.enqueue(a.clone(), false);
        app.enqueue(b.clone(), false);
        assert_eq!(fx.spawned(), ["a"], "b waits");
        assert_eq!(status_of(&app, &b), RowStatus::Queued);
        assert!(matches!(status_of(&app, &a), RowStatus::Transcribing(_)));

        settle(&mut app); // a done, b starts
        assert_eq!(fx.spawned(), ["a", "b"]);
        settle(&mut app);
        assert_eq!(status_of(&app, &a), RowStatus::Transcribed);
        assert_eq!(status_of(&app, &b), RowStatus::Transcribed);
        assert!(!app.busy());
    }

    #[test]
    fn enqueueing_twice_does_not_run_twice() {
        let fx = Fixture::new();
        let a = fx.recording("a");
        let mut app = fx.app(fx.spawner(0, true, None));
        app.enqueue(a.clone(), false);
        app.enqueue(a, false);
        settle(&mut app);
        assert_eq!(fx.spawned(), ["a"]);
    }

    #[test]
    fn a_failed_job_shows_a_reason_and_can_be_retried() {
        let fx = Fixture::new();
        let a = fx.recording("a");
        let mut app = fx.app(fx.spawner(1, false, None));
        app.enqueue(a.clone(), false);
        settle(&mut app);
        let RowStatus::Failed(why) = status_of(&app, &a) else { panic!("{:?}", status_of(&app, &a)) };
        assert!(why.contains("exited"), "{why}");
        app.enqueue(a.clone(), false);
        assert!(matches!(status_of(&app, &a), RowStatus::Transcribing(_)), "retry starts again");
        assert_eq!(fx.spawned(), ["a", "a"]);
        settle(&mut app);
    }

    #[test]
    fn the_helpers_own_failure_message_wins_over_the_exit_code() {
        let fx = Fixture::new();
        let a = fx.recording("a");
        let mut app = fx.app(fx.spawner(1, false, Some("ValueError: bad wav")));
        app.enqueue(a.clone(), false);
        settle(&mut app);
        assert_eq!(status_of(&app, &a), RowStatus::Failed("ValueError: bad wav".into()));
    }

    #[test]
    fn a_clean_exit_without_a_transcript_is_a_failure() {
        let fx = Fixture::new();
        let a = fx.recording("a");
        let mut app = fx.app(fx.spawner(0, false, None));
        app.enqueue(a.clone(), false);
        settle(&mut app);
        assert!(matches!(status_of(&app, &a), RowStatus::Failed(_)));
    }

    #[test]
    fn a_failure_recorded_by_an_earlier_window_is_shown_after_reopening() {
        let fx = Fixture::new();
        let a = fx.recording("a");
        let state =
            JobState { recording: a.clone(), pid: 0, phase: Phase::Failed, progress: 0.1, message: "no GPU".into() };
        progress::write(&state_path(&fx.cache(), "old"), &state).unwrap();
        let app = fx.app(fx.spawner(0, true, None));
        assert_eq!(status_of(&app, &a), RowStatus::Failed("no GPU".into()));
    }

    #[test]
    fn a_job_from_another_window_keeps_ours_waiting_until_it_ends() {
        let fx = Fixture::new();
        let (a, b) = (fx.recording("a"), fx.recording("b"));
        let theirs = JobState {
            recording: a.clone(),
            pid: std::process::id(),
            phase: Phase::Running,
            progress: 0.5,
            message: String::new(),
        };
        let record = state_path(&fx.cache(), "theirs");
        progress::write(&record, &theirs).unwrap();
        let mut app = fx.app(fx.spawner(0, true, None));
        assert_eq!(status_of(&app, &a), RowStatus::Transcribing(0.5));
        assert!(app.busy());

        app.enqueue(b.clone(), false);
        assert!(fx.spawned().is_empty());
        assert_eq!(status_of(&app, &b), RowStatus::Queued);

        std::fs::remove_file(&record).unwrap(); // their job finished
        app.poll();
        assert_eq!(fx.spawned(), ["b"]);
        settle(&mut app);
    }

    #[test]
    fn removing_a_queued_job_and_cancelling_a_running_one() {
        let fx = Fixture::new();
        let (a, b) = (fx.recording("a"), fx.recording("b"));
        let slow: Spawner = {
            let spawned = fx.spawned.clone();
            Box::new(move |rec, _, _, _| {
                spawned.borrow_mut().push(rec.file_stem().unwrap().to_string_lossy().into_owned());
                Command::new("cmd").args(["/C", "ping -n 60 127.0.0.1 > nul"]).spawn()
            })
        };
        let mut app = fx.app(slow);
        app.enqueue(a.clone(), false);
        app.enqueue(b.clone(), false);
        app.cancel(&b);
        assert_eq!(status_of(&app, &b), RowStatus::NotTranscribed);

        let job_dir = fx.cache().tmp_job_dir(&app.running.as_ref().unwrap().job_id);
        std::fs::create_dir_all(&job_dir).unwrap();
        app.cancel(&a);
        assert!(app.running.is_none() && !job_dir.exists());
        assert_eq!(status_of(&app, &a), RowStatus::NotTranscribed);
        assert_eq!(fx.spawned(), ["a"], "b was removed before it could start");
    }

    #[test]
    fn a_job_that_cannot_start_is_a_failure() {
        let fx = Fixture::new();
        let a = fx.recording("a");
        let mut app = fx.app(Box::new(|_, _, _, _| Err(std::io::Error::other("no exe"))));
        app.enqueue(a.clone(), false);
        assert_eq!(status_of(&app, &a), RowStatus::Failed("could not start: no exe".into()));
        assert!(!app.busy());
    }

    #[test]
    fn status_text_matches_the_badges() {
        assert_eq!(RowStatus::NotTranscribed.text(), "Not transcribed");
        assert_eq!(RowStatus::Transcribing(0.42).text(), "Transcribing 42%");
        assert_eq!(RowStatus::Transcribed.text(), "Transcribed");
        assert_eq!(RowStatus::Failed("x".into()).text(), "Failed (x)");
    }

    /// Two frames of the whole window; the top bar must stay inside it.
    fn lay_out(app: &mut TranscriptsApp) {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::from(WINDOW_SIZE));
        let ctx = egui::Context::default();
        let mut bar = egui::Rect::NOTHING;
        for _ in 0..2 {
            let input = egui::RawInput { screen_rect: Some(screen), ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| bar = app.layout(ui));
            out.textures_delta.clear(); // no renderer to hand the font atlas to
        }
        assert!(screen.contains_rect(bar), "top bar {bar:?} outside window {screen:?}");
    }

    #[test]
    fn rows_and_bars_lay_out_inside_the_window() {
        let fx = Fixture::new();
        for i in 0..30 {
            fx.recording(&format!("2026-10-19_10{i:02}_Chrome"));
        }
        let done = fx.recording("2026-10-19_1100_Teams");
        std::fs::write(md_path(&done), "# Transcript").unwrap();
        let mut app = fx.app(fx.spawner(0, true, None));
        let first = app.items[0].path.clone();
        app.enqueue(first, false);
        app.failures.insert(app.items[1].path.clone(), "something went wrong with a long explanation".into());
        app.confirm = Some(done);
        lay_out(&mut app);
        settle(&mut app);
    }

    #[test]
    fn an_empty_folder_lays_out() {
        let fx = Fixture::new();
        let mut app = fx.app(fx.spawner(0, true, None));
        lay_out(&mut app);
    }
}
