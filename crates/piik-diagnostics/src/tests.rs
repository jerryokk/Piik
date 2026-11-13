use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tracing::Level;
use tracing_subscriber::Registry;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;

use crate::recorder::MAX_FILE_BYTES;
use crate::{Error, Json, LineWriter, Recorder, RedactingLayer};

fn directory() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn with_recorder<T>(recorder: &Recorder, body: impl FnOnce() -> T) -> T {
    tracing::subscriber::with_default(Registry::default().with(recorder.layer()), body)
}

fn read_archive(path: &Path) -> Vec<(String, Vec<u8>)> {
    let mut archive = zip::ZipArchive::new(fs::File::open(path).unwrap()).unwrap();
    (0..archive.len())
        .map(|index| {
            let mut entry = archive.by_index(index).unwrap();
            let mut data = Vec::new();
            entry.read_to_end(&mut data).unwrap();
            (entry.name().unwrap().into_owned(), data)
        })
        .collect()
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

#[test]
fn recorder_rotation_and_export() {
    let directory = directory();
    const PRIVATE: &str = "private-environment-and-media-sentinel";
    fs::write(directory.path().join("private-config.json"), PRIVATE).unwrap();
    let recorder = Recorder::open(directory.path(), "client", "test-revision").unwrap();
    let chunk = vec![b'x'; (MAX_FILE_BYTES / 2) as usize];
    for _ in 0..5 {
        recorder.with_log(|log| log.write(&chunk)).unwrap();
    }
    with_recorder(&recorder, || {
        tracing::debug!(event = "started", "capture state")
    });
    let log = recorder.log_path();
    let backup = log.with_extension("log.1");
    for path in [&log, &backup] {
        assert!(
            fs::metadata(path).unwrap().len() <= MAX_FILE_BYTES,
            "bounded log {path:?}"
        );
    }
    assert!(
        !log.with_extension("log.2").exists(),
        "unexpected second backup"
    );

    let archive_path = recorder.export().unwrap();
    let mut expected = [
        "client.log",
        "client.log.1",
        "metadata.json",
        "memory.json",
        "context.json",
        "README.txt",
    ]
    .map(|name| (name, false));
    for (name, data) in read_archive(&archive_path) {
        let found = expected.iter_mut().find(|(expected, _)| *expected == name);
        found
            .unwrap_or_else(|| panic!("unexpected archive entry: {name}"))
            .1 = true;
        assert!(!contains(&data, PRIVATE), "private data in {name}");
        match name.as_str() {
            "client.log" => assert!(
                contains(&data, r#""level":"DEBUG""#),
                "debug event was not persisted"
            ),
            "metadata.json" => {
                let metadata: Value = serde_json::from_slice(&data).unwrap();
                assert_eq!(metadata["revision"], "test-revision");
                assert_eq!(metadata["component"], "client");
                assert_eq!(metadata["os"], std::env::consts::OS);
                assert_ne!(metadata["reportId"], "");
                assert_eq!(metadata["partial"], true);
            }
            "memory.json" => {
                let memory: Value = serde_json::from_slice(&data).unwrap();
                assert!(
                    memory["physicalBytes"].as_u64().unwrap() > 0,
                    "invalid memory counters"
                );
            }
            _ => {}
        }
    }
    for (name, found) in expected {
        assert!(found, "missing archive entry: {name}");
    }
    let second = recorder.export().unwrap();
    assert_ne!(second, archive_path, "export must have a unique path");
    assert_eq!(recorder.last_export_path(), Some(second));
    recorder.close().unwrap();

    let reopened = Recorder::open(directory.path(), "client", "next-process").unwrap();
    assert_eq!(
        reopened.with_log(|log| log.retention())["earlierHistoryMayBeMissing"],
        true,
        "restart incorrectly claimed complete prior history"
    );
    reopened.close().unwrap();
}

#[test]
fn recorder_keeps_the_last_successful_export_after_failure_and_close() {
    let directory = directory();
    let recorder = Recorder::open(directory.path(), "client", "test").unwrap();
    assert_eq!(recorder.last_export_path(), None);
    let saved = recorder.export().unwrap();
    // A regular file in the directory position makes archive creation fail.
    let original = recorder.with_log(|log| {
        let original = log.path.clone();
        log.path = original.join("unavailable").join("client.log");
        original
    });
    let failed = recorder.export();
    recorder.with_log(|log| log.path = original);
    assert!(failed.is_err());
    assert_eq!(
        recorder.last_export_path().as_ref(),
        Some(&saved),
        "failed export replaced the saved report"
    );
    recorder.close().unwrap();
    assert!(matches!(recorder.export(), Err(Error::Closed)));
    assert_eq!(
        recorder.last_export_path().as_ref(),
        Some(&saved),
        "closed recorder lost the saved report"
    );
    read_archive(&saved);
}

#[test]
fn recorder_close_waits_for_an_export_already_in_progress() {
    let directory = directory();
    let recorder = Recorder::open(directory.path(), "client", "test").unwrap();
    let (started_tx, started) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    recorder.set_barrier(move || {
        let _ = started_tx.send(());
        let _ = release_rx.lock().unwrap().recv();
    });
    let exporter = recorder.clone();
    let exported = std::thread::spawn(move || exporter.export());
    started
        .recv_timeout(Duration::from_secs(5))
        .expect("export did not reach the archive writer");
    let closer = recorder.clone();
    let (closed_tx, closed) = mpsc::channel();
    std::thread::spawn(move || closed_tx.send(closer.close()));
    assert!(
        closed.recv_timeout(Duration::from_millis(25)).is_err(),
        "close returned before the archive finished"
    );
    release.send(()).unwrap();
    let path = exported.join().unwrap().unwrap();
    closed.recv().unwrap().unwrap();
    assert_eq!(
        recorder.last_export_path(),
        Some(path.clone()),
        "close lost the completed report"
    );
    read_archive(&path);
}

#[test]
fn recorder_close_and_failures() {
    assert!(
        Recorder::open(directory().path(), "../client", "test").is_err(),
        "accepted a component path"
    );
    let directory = directory();
    let recorder = Recorder::open(directory.path(), "server", "test").unwrap();
    let dispatch = tracing::Dispatch::new(Registry::default().with(recorder.layer()));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let barrier = Arc::new(std::sync::Barrier::new(5));
    let workers: Vec<_> = (0..4)
        .map(|worker| {
            let (dispatch, stop, barrier) = (dispatch.clone(), stop.clone(), barrier.clone());
            std::thread::spawn(move || {
                tracing::dispatcher::with_default(&dispatch, || {
                    tracing::debug!(worker, "concurrent log");
                    barrier.wait();
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        tracing::debug!(worker, "concurrent log");
                    }
                });
            })
        })
        .collect();
    barrier.wait();
    recorder.export().unwrap();
    let concurrent = recorder.clone();
    let exporter = std::thread::spawn(move || match concurrent.export() {
        Ok(_) | Err(Error::Closed) => {}
        Err(error) => panic!("concurrent export: {error}"),
    });
    recorder.close().unwrap();
    recorder.close().unwrap();
    assert!(
        matches!(recorder.export(), Err(Error::Closed)),
        "export after close"
    );
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    for worker in workers {
        worker.join().unwrap();
    }
    exporter.join().unwrap();

    let directory = self::directory();
    let failing = Recorder::open(directory.path(), "client", "test").unwrap();
    failing.with_log(|log| log.file = None);
    with_recorder(&failing, || tracing::error!("write fails"));
    let partial = failing
        .export()
        .expect("one failed log writer discarded usable runtime evidence");
    let metadata = read_archive(&partial)
        .into_iter()
        .find(|(name, _)| name == "metadata.json")
        .unwrap()
        .1;
    let metadata: Value = serde_json::from_slice(&metadata).unwrap();
    assert_eq!(metadata["partial"], true);
    assert!(
        !metadata["collectorErrors"].as_array().unwrap().is_empty(),
        "partial export hid failed collection"
    );
    assert!(
        matches!(failing.close(), Err(Error::Closed)),
        "close hid a log write failure"
    );
}

#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl Buffer {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl Write for Buffer {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Buffer {
    type Writer = Buffer;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[test]
fn dependency_logs_respect_diagnostic_opt_in() {
    let directory = directory();
    let recorder = Recorder::open(directory.path(), "client", "test").unwrap();
    const SECRET: &str = "must-not-appear-in-report";
    let console = Buffer::default();
    let subscriber = Registry::default()
        .with(RedactingLayer::console(console.clone()))
        .with(recorder.layer());
    tracing::subscriber::with_default(subscriber, || {
        // Reject packet traces before even evaluating their payloads.
        let evaluated = std::cell::Cell::new(false);
        tracing::trace!(target: "str0m", packet = {
            evaluated.set(true);
            "unwanted packet payload"
        });
        assert!(!evaluated.get());
        assert!(!tracing::enabled!(target: "str0m", Level::TRACE));
        // A bare probe must not cost the next console event.
        let _ = tracing::enabled!(target: "piik_app", Level::DEBUG);
        tracing::error!(target: "str0m::ice", "dropping candidate with ufrag {SECRET} because it doesn't match");
        let failure = std::io::Error::other("capture process stopped unexpectedly");
        tracing::error!(target: "piik_app", error = &failure as &dyn std::error::Error, "capture process failed");
        tracing::debug!(target: "piik_app", "application debug detail");
        let mut writer = LineWriter::new("stdlib");
        writer.write_all(b"dependency stderr line\n").unwrap();
    });
    let console = console.text();
    assert!(
        !console.contains("dropping candidate")
            && !console.contains(SECRET)
            && !console.contains("application debug detail")
            && !console.contains("dependency stderr line")
            && console.contains("capture process stopped unexpectedly"),
        "console logging lost the owning failure or exposed dependency details: {console}"
    );
    let data = fs::read(recorder.log_path()).unwrap();
    assert!(
        !contains(&data, SECRET)
            && !contains(&data, "unwanted packet payload")
            && contains(&data, "application debug detail")
            && contains(&data, "dropping candidate with ufrag [redacted]"),
        "debug logging lost the dependency cause or retained its ICE credential"
    );
    assert!(
        contains(&data, r#""library":"str0m::ice""#) && contains(&data, "dependency stderr line")
    );
    recorder.close().unwrap();
}

#[test]
fn dependency_diagnostics_preserve_causes_without_secrets() {
    let directory = directory();
    let recorder = Recorder::open(directory.path(), "client", "test").unwrap();
    const SECRET: &str = "must-not-appear-in-report";
    let hex_secret: String = SECRET.bytes().map(|byte| format!("{byte:02x}")).collect();
    with_recorder(&recorder, || {
        #[derive(Debug, thiserror::Error)]
        #[error("interface enumeration")]
        struct Enumeration(#[source] std::io::Error);
        // WSAEAFNOSUPPORT, reported by Windows socket enumeration.
        let failure = Enumeration(std::io::Error::from_raw_os_error(10047));
        tracing::error!(
            error = &failure as &dyn std::error::Error,
            requestId = "request-17",
            nested = ?Json(json!({"instanceToken": SECRET, "state": "checking", "stats": {"jitter": 12, "password": SECRET}})),
            trackId = "private-track-identity",
            "operation"
        );
        tracing::debug!(target: "str0m::ice", "Started agent: remoteUfrag: {SECRET:?}, remotePwd: {SECRET:?}");
        tracing::warn!(target: "str0m::ice", "mismatch username expected({hex_secret}) actual({hex_secret})");
        tracing::error!(target: "str0m::pc", "dropping candidate with ufrag {SECRET} because it doesn't match");
        tracing::warn!(target: "str0m::ice", "Failed to get TCP connections by ufrag: tcp4 127.0.0.1:1 {SECRET}");
        tracing::debug!(target: "str0m::pc", "got new track: &{{id:{SECRET} streamID:private-track-identity peeked:[1 2]}}");
        let mut writer = LineWriter::new("capture");
        for part in [
            "stage=device-open\nAuthorization: Bear".to_owned(),
            format!("er {SECRET}\n"),
            "-----BEGIN PRIVATE KEY-----\n".to_owned(),
            format!("{SECRET}\n-----END PRIVATE KEY-----\n"),
            format!("https://example.test/r/1234#v={SECRET}\n"),
            "hresult=0x80070005".to_owned(),
        ] {
            writer.write_all(part.as_bytes()).unwrap();
        }
        writer.close();
    });
    let data = fs::read(recorder.log_path()).unwrap();
    for forbidden in [SECRET, &hex_secret, "private-track-identity"] {
        assert!(
            !contains(&data, forbidden),
            "sensitive value retained: {forbidden:?}"
        );
    }
    for useful in [
        "request-17",
        "interface enumeration",
        "systemCode",
        "device-open",
        "0x80070005",
        "checking",
        r#""jitter":12"#,
    ] {
        assert!(
            contains(&data, useful),
            "missing diagnostic context: {useful:?}"
        );
    }
    recorder.close().unwrap();
}

#[test]
fn identities_are_stable_opaque_hashes() {
    let first = crate::id("peer-1");
    assert!(first.starts_with("id-") && first.len() == 19);
    assert_eq!(crate::id(&first), first);
    assert_eq!(crate::id(""), "");
    assert_eq!(
        crate::safe_text("wss://user:pass@host/x"),
        "wss://[redacted]@host/x"
    );
}
