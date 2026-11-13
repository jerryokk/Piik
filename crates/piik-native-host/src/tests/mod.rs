//! Native Host tests against `examples/host_capture_fixture.rs` and loopback
//! media.

mod audio;
mod capture_state;
mod session;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures_util::future::BoxFuture;
use piik_native_capture::{CaptureTarget, VideoOptions, VideoProfile};
use piik_native_control::media::{MediaEvent, MediaEventKind, QualityProfile};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::bridge::EventSink;
use crate::capture::ProcessCapture;
use crate::session::{Options, Session};

/// Every fixture test runs a real capture child process. In parallel they
/// starve each other of CPU, and their fixed startup and shutdown deadlines
/// expire with no logic having failed. Running them one at a time is the
/// property under test here: a share owns the machine's capture devices.
///
/// The guard lives in the fixture so a new test cannot forget to take it. A
/// panic releases it through the same drop, and the poison is ignored because
/// the lock protects scheduling, not shared data.
static SERIAL: Mutex<()> = Mutex::new(());

/// One test's private copy of the fixture with its own settings file.
pub(crate) struct Fixture {
    pub dir: tempfile::TempDir,
    pub executable: PathBuf,
    // Declared last so the directory and executable are released first.
    _serial: MutexGuard<'static, ()>,
}

impl Fixture {
    pub fn new(mode: &str) -> Self {
        // Blocking here is safe: the holder never waits on this test, so the
        // queue always makes progress.
        let serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let test = std::env::current_exe().expect("test executable");
        // target/<profile>/deps/<test>.exe -> target/<profile>/examples/<fixture>.exe
        let source = test
            .parent()
            .and_then(Path::parent)
            .map(|profile| profile.join("examples").join("host_capture_fixture.exe"))
            .expect("target directory");
        assert!(
            source.exists(),
            "build the fixture with `cargo test -p piik-native-host`: {}",
            source.display()
        );
        let dir = tempfile::tempdir().expect("fixture directory");
        let executable = dir.path().join("host_capture_fixture.exe");
        std::fs::copy(&source, &executable).expect("copy fixture");
        let fixture = Self {
            dir,
            executable,
            _serial: serial,
        };
        fixture.set("PIIK_HOST_FIXTURE", mode);
        fixture
    }

    /// Sets one setting for processes started from now on.
    pub fn set(&self, key: &str, value: &str) {
        let path = self.dir.path().join("fixture.env");
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        let mut lines: Vec<String> = current
            .lines()
            .filter(|line| line.split_once('=').is_none_or(|(name, _)| name != key))
            .map(str::to_owned)
            .collect();
        if !value.is_empty() {
            lines.push(format!("{key}={value}"));
        }
        std::fs::write(path, lines.join("\n")).expect("write fixture settings");
    }

    pub fn path(&self, name: &str) -> String {
        self.dir.path().join(name).to_string_lossy().into_owned()
    }

    pub fn capture(&self) -> Arc<ProcessCapture> {
        Arc::new(ProcessCapture {
            executable: self.executable.clone(),
        })
    }
}

/// Test events: a bounded channel the test drains.
pub(crate) struct Events(pub mpsc::Sender<MediaEvent>);

impl EventSink for Events {
    fn send(&self, event: MediaEvent) -> BoxFuture<'_, bool> {
        Box::pin(async move { self.0.send(event).await.is_ok() })
    }
}

pub(crate) fn events() -> (Arc<dyn EventSink>, mpsc::Receiver<MediaEvent>) {
    let (tx, rx) = mpsc::channel(256);
    (Arc::new(Events(tx)), rx)
}

pub(crate) fn display() -> CaptureTarget {
    CaptureTarget {
        kind: "display".into(),
        source_id: "1".into(),
        title: "Fixture".into(),
        ..CaptureTarget::default()
    }
}

pub(crate) fn recovery_profile() -> VideoProfile {
    VideoProfile {
        width: 1280,
        height: 720,
        framerate: 30,
        bitrate: 2_000_000,
        preference: "balanced".into(),
    }
}

pub(crate) fn video_options() -> VideoOptions {
    VideoOptions {
        codec: "vp8".into(),
        profile: recovery_profile(),
        target: display(),
        ..VideoOptions::default()
    }
}

pub(crate) fn options(fixture: &Fixture, events: Arc<dyn EventSink>) -> Options {
    let video = video_options();
    Options {
        share_id: "share_fixture".into(),
        capture: fixture.capture(),
        profile: QualityProfile {
            video: video.profile.clone(),
            audio_bitrate: 64_000,
        },
        video,
        audio_enabled: false,
        exclude_audio: None,
        microphone_mixing: false,
        edge_capacity: 1,
        bind_address: "127.0.0.1:0".into(),
        port_mapping: false,
        remote_input: None,
        events,
    }
}

pub(crate) async fn start(options: Options) -> (Session, CancellationToken) {
    let parent = CancellationToken::new();
    let session = tokio::time::timeout(Duration::from_secs(10), Session::start(&parent, options))
        .await
        .expect("start did not finish")
        .expect("share did not start");
    (session, parent)
}

/// The next event matching `select`, within `limit`.
pub(crate) async fn next_event<T>(
    events: &mut mpsc::Receiver<MediaEvent>,
    limit: Duration,
    mut select: impl FnMut(&MediaEvent) -> Option<T>,
) -> Option<T> {
    tokio::time::timeout(limit, async {
        while let Some(event) = events.recv().await {
            if let Some(value) = select(&event) {
                return Some(value);
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
}

pub(crate) fn audio_state(event: &MediaEvent) -> Option<(bool, bool, bool)> {
    match event.kind {
        MediaEventKind::AudioState {
            source_audio,
            microphone,
            failed,
        } => Some((source_audio, microphone, failed)),
        _ => None,
    }
}

pub(crate) async fn eventually(label: &str, mut predicate: impl FnMut() -> bool) {
    for _ in 0..500 {
        if predicate() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{label} did not happen");
}
