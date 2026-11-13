//! Process-level tunnel tests. This binary is also the cloudflared fixture:
//! each test copies it into a temporary directory beside a `fixture.json` that
//! selects the fixture's behavior.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use piik_failure::Reason;
use piik_public_tunnel::{Error, Startup};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, SubscriberExt};

const LOCAL: &str = "http://127.0.0.1:8787";

type TestFn = fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>;

fn main() {
    if let Some(fixture) = fixture_config() {
        run_fixture(&fixture);
    }
    install_hooks();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let filter: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .collect();
    let tests: [(&str, TestFn); 9] = [
        ("startup_retains_plain_dependency_failure", || {
            Box::pin(startup_retains_plain_dependency_failure())
        }),
        (
            "startup_deadline_retains_sanitized_dependency_cause",
            || Box::pin(startup_deadline_retains_sanitized_dependency_cause()),
        ),
        ("startup_retries_only_retired_pre_ready_children", || {
            Box::pin(startup_retries_only_retired_pre_ready_children())
        }),
        ("startup_retries_share_cancellation_and_deadline", || {
            Box::pin(startup_retries_share_cancellation_and_deadline())
        }),
        ("startup_enables_cloudflared_protocol_fallback", || {
            Box::pin(startup_enables_cloudflared_protocol_fallback())
        }),
        ("close_keeps_an_observed_failure_while_logs_drain", || {
            Box::pin(close_keeps_an_observed_failure_while_logs_drain())
        }),
        ("ready_tunnel_waits_for_its_explicit_ordered_close", || {
            Box::pin(ready_tunnel_waits_for_its_explicit_ordered_close())
        }),
        ("startup_cancellation_still_retires_the_tunnel", || {
            Box::pin(startup_cancellation_still_retires_the_tunnel())
        }),
        ("dropped_tunnel_is_retired", || {
            Box::pin(dropped_tunnel_is_retired())
        }),
    ];
    let mut failed = Vec::new();
    let mut ran = 0;
    for (name, test) in tests {
        if !filter.is_empty() && !filter.iter().any(|wanted| name.contains(wanted.as_str())) {
            continue;
        }
        ran += 1;
        print!("test {name} ... ");
        let _ = std::io::stdout().flush();
        set_hook(Hook::None);
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| runtime.block_on(test())));
        set_hook(Hook::None);
        match outcome {
            Ok(()) => println!("ok"),
            Err(_) => {
                println!("FAILED");
                failed.push(name);
            }
        }
    }
    println!(
        "\ntest result: {}. {} passed; {} failed",
        if failed.is_empty() { "ok" } else { "FAILED" },
        ran - failed.len(),
        failed.len()
    );
    drop(runtime);
    std::process::exit(i32::from(!failed.is_empty()));
}

// ---------------------------------------------------------------------------
// Fixture

fn fixture_config() -> Option<Value> {
    let executable = std::env::current_exe().ok()?;
    let payload = std::fs::read(executable.parent()?.join("fixture.json")).ok()?;
    serde_json::from_slice(&payload).ok()
}

fn sleep_forever() -> ! {
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

fn run_fixture(config: &Value) -> ! {
    let mode = config["mode"].as_str().unwrap_or_default();
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| {
        args.iter()
            .position(|arg| arg == name)
            .and_then(|index| args.get(index + 1))
            .cloned()
    };
    if mode == "deadline-detail" {
        eprintln!(
            r#"{{"level":"error","message":"edge discovery failed","error":"DNS lookup failed token=fixture-secret"}}"#
        );
        eprintln!(r#"{{"level":"info","message":"retrying"}}"#);
        sleep_forever();
    }
    if mode == "plain-exit" {
        eprintln!("failed to request quick Tunnel: connect timeout token=fixture-secret");
        println!("trailing ordinary output");
        std::process::exit(7);
    }
    if let Some(record) = config["attempts"].as_str() {
        let previous = std::fs::read_to_string(record).unwrap_or_default();
        let paths: Vec<&str> = previous
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        if paths.iter().any(|path| Path::new(path.trim()).exists()) {
            eprintln!("previous attempt was not retired");
            std::process::exit(9);
        }
        let Some(config_path) = flag("--config") else {
            std::process::exit(9)
        };
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(record)
            .and_then(|mut file| writeln!(file, "{config_path}"));
        if written.is_err() {
            std::process::exit(9);
        }
        let failures = config["failures"].as_u64().unwrap_or(0) as usize;
        if paths.len() < failures {
            eprintln!(r#"{{"level":"error","message":"older recovered failure"}}"#);
            eprintln!(
                r#"{{"level":"error","message":"fixture allocation unavailable","error":"connect timeout token=fixture-secret"}}"#
            );
            eprintln!(r#"{{"level":"warn","message":"cleanup warning"}}"#);
            eprintln!(r#"{{"level":"info","message":"cleanup complete"}}"#);
            std::process::exit(config["exit_code"].as_i64().unwrap_or(0) as i32);
        }
    }
    if mode == "tcp-only" {
        // Model the Quick Tunnel's opt-in protocol fallback. Without both flags,
        // an unavailable UDP edge consumes the startup budget.
        for (name, want) in [("--protocol", "auto"), ("--max-edge-addr-retries", "0")] {
            if flag(name).as_deref() != Some(want) {
                eprintln!("UDP unavailable; TCP fallback not enabled within startup budget");
                std::process::exit(7);
            }
        }
    }
    if matches!(mode, "ready" | "fail-after-ready" | "tcp-only") {
        println!(r#"{{"message":"https://test-room.trycloudflare.com"}}"#);
        println!(r#"{{"message":"Registered tunnel connection"}}"#);
    }
    if mode == "fail-after-ready" {
        println!(r#"{{"level":"fatal","message":"fixture exit failure"}}"#);
        let exit = PathBuf::from(config["exit"].as_str().unwrap_or_default());
        loop {
            if exit.exists() {
                std::process::exit(7);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    eprintln!("fixture waiting");
    sleep_forever();
}

struct Fixture {
    /// Owns the copied fixture executable and its configuration.
    _directory: tempfile::TempDir,
    executable: PathBuf,
}

impl Fixture {
    fn new(config: Value) -> Self {
        // Copies stay under target/, beside the test binary, not in %TEMP%.
        let target = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
            .expect("test executable directory");
        let directory = tempfile::tempdir_in(target).expect("fixture directory");
        // A neutral name: an unsigned test binary copied into %TEMP% under a
        // well-known tool's name is a malware heuristic (Defender flagged it).
        let executable = directory.path().join("tunnel-fixture.exe");
        std::fs::copy(
            std::env::current_exe().expect("test executable"),
            &executable,
        )
        .expect("copy fixture");
        std::fs::write(directory.path().join("fixture.json"), config.to_string())
            .expect("write fixture");
        Self {
            _directory: directory,
            executable,
        }
    }

    fn startup(&self) -> Startup {
        Startup::new(&self.executable, LOCAL)
    }
}

fn assert_retired_attempts(record: &str, count: usize) {
    let payload = std::fs::read_to_string(record).expect("attempt record");
    let paths: Vec<&str> = payload
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(
        paths.len(),
        count,
        "started {} children, want {count}",
        paths.len()
    );
    for path in paths {
        assert!(
            !Path::new(path.trim()).exists(),
            "attempt config was not removed: {path}"
        );
    }
}

fn assert_empty(directory: &Path) {
    let entries: Vec<_> = std::fs::read_dir(directory).expect("read dir").collect();
    assert!(
        entries.is_empty(),
        "child retained caller config: {entries:?}"
    );
}

// ---------------------------------------------------------------------------
// Tracing hooks: trigger actions at observed lifecycle boundaries rather than
// at assumed launch times.

enum Hook {
    None,
    /// Cancels when a field has the given value.
    Cancel {
        key: &'static str,
        value: &'static str,
        token: CancellationToken,
    },
    /// Blocks the logging thread on a matching field until released, and
    /// reports a second field match.
    Block {
        value: &'static str,
        entered: CancellationToken,
        release: Arc<(Mutex<bool>, std::sync::Condvar)>,
        exited: CancellationToken,
    },
}

static HOOK: OnceLock<Mutex<Hook>> = OnceLock::new();

fn set_hook(hook: Hook) {
    if let Ok(mut current) = HOOK.get_or_init(|| Mutex::new(Hook::None)).lock() {
        *current = hook;
    }
}

#[derive(Default)]
struct Fields(Vec<(String, String)>);

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.push((field.name().to_owned(), value.to_owned()));
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.push((field.name().to_owned(), format!("{value:?}")));
    }
}

struct HookLayer;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for HookLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let has = |key: &str, value: &str| fields.0.iter().any(|(k, v)| k == key && v == value);
        let release = {
            let Some(Ok(hook)) = HOOK.get().map(Mutex::lock) else {
                return;
            };
            match &*hook {
                Hook::None => None,
                Hook::Cancel { key, value, token } => {
                    if has(key, value) {
                        token.cancel();
                    }
                    None
                }
                Hook::Block {
                    value,
                    entered,
                    release,
                    exited,
                } => {
                    if has("event", "child-exited") {
                        exited.cancel();
                    }
                    if has("output", value) {
                        entered.cancel();
                        Some(Arc::clone(release))
                    } else {
                        None
                    }
                }
            }
        };
        if let Some(release) = release {
            let (lock, ready) = &*release;
            let mut released = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            while !*released {
                released = ready
                    .wait(released)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }
    }
}

fn install_hooks() {
    let subscriber = tracing_subscriber::registry().with(HookLayer);
    let _ = tracing::subscriber::set_global_default(subscriber);
}

// ---------------------------------------------------------------------------
// Tests

async fn startup_retains_plain_dependency_failure() {
    let fixture = Fixture::new(json!({"mode": "plain-exit"}));
    let configs = tempfile::tempdir().unwrap();
    let error = fixture
        .startup()
        .temporary_directory(configs.path())
        .start(&CancellationToken::new())
        .await
        .unwrap_err();
    assert!(
        error.startup_exited() && error.exit_code() == Some(7),
        "plain dependency failure lost its exit status: {error}"
    );
    assert_eq!(error.reason(), Some(Reason::TunnelExited));
    let text = error.to_string();
    assert!(
        text.contains("failed to request quick Tunnel: connect timeout token=[redacted]")
            && !text.contains("fixture-secret")
            && !text.contains("ordinary output"),
        "plain dependency failure was omitted, unsafe or overwritten: {text}"
    );
    assert_empty(configs.path());
}

async fn startup_deadline_retains_sanitized_dependency_cause() {
    let fixture = Fixture::new(json!({"mode": "deadline-detail"}));
    let configs = tempfile::tempdir().unwrap();
    let error = fixture
        .startup()
        .temporary_directory(configs.path())
        .timeout(Duration::from_secs(2))
        .start(&CancellationToken::new())
        .await
        .unwrap_err();
    assert!(
        error.is_deadline_exceeded(),
        "startup deadline was lost: {error}"
    );
    assert_eq!(error.reason(), Some(Reason::TunnelTimeout));
    let text = error.to_string();
    assert!(
        text.contains("edge discovery failed\nDNS lookup failed token=[redacted]")
            && !text.contains("fixture-secret")
            && !text.contains("retrying"),
        "startup deadline lost or exposed dependency cause: {text}"
    );
    assert_empty(configs.path());
}

async fn startup_retries_only_retired_pre_ready_children() {
    for (name, failures, exit_code, ready) in [
        ("fourth attempt succeeds", 3, 7, true),
        ("four failures stop", 9, 7, false),
        ("clean exit is not readiness", 9, 0, false),
    ] {
        let probe = tempfile::tempdir().unwrap();
        let record = probe.path().join("attempts").to_string_lossy().into_owned();
        let fixture = Fixture::new(json!({
            "mode": "ready", "attempts": record, "failures": failures, "exit_code": exit_code,
        }));
        let result = fixture.startup().start(&CancellationToken::new()).await;
        if ready {
            let process = result.unwrap_or_else(|error| panic!("{name}: {error}"));
            assert_eq!(process.origin(), "https://test-room.trycloudflare.com");
            process
                .close()
                .await
                .unwrap_or_else(|error| panic!("{name}: {error}"));
        } else {
            let error = result.err().unwrap_or_else(|| panic!("{name}: started"));
            assert!(error.startup_exited(), "{name}: {error}");
            assert_eq!(error.reason(), Some(Reason::TunnelExited), "{name}");
            if exit_code != 0 {
                assert_eq!(
                    error.exit_code(),
                    Some(exit_code),
                    "{name}: latest dependency exit was lost"
                );
                let text = error.to_string();
                assert!(
                    text.contains(
                        "fixture allocation unavailable\nconnect timeout token=[redacted]"
                    ) && !text.contains("fixture-secret")
                        && !text.contains("older recovered failure")
                        && !text.contains("cleanup"),
                    "{name}: latest sanitized dependency cause was lost or replaced: {text}"
                );
            }
        }
        assert_retired_attempts(&record, 4);
    }
}

async fn startup_retries_share_cancellation_and_deadline() {
    for (name, key, value, attempts) in [
        ("during retry delay", "event", "startup-retry", 1),
        ("during second child", "output", "fixture waiting", 2),
    ] {
        let probe = tempfile::tempdir().unwrap();
        let record = probe.path().join("attempts").to_string_lossy().into_owned();
        let fixture = Fixture::new(json!({
            "mode": "starting", "attempts": record, "failures": 1, "exit_code": 7,
        }));
        let stop = CancellationToken::new();
        set_hook(Hook::Cancel {
            key,
            value,
            token: stop.clone(),
        });
        let result = fixture
            .startup()
            .timeout(Duration::from_secs(10))
            .start(&stop)
            .await;
        set_hook(Hook::None);
        let error = result.err().unwrap_or_else(|| panic!("{name}: started"));
        assert!(error.is_canceled(), "{name}: startup cancellation: {error}");
        assert_retired_attempts(&record, attempts);
    }

    let probe = tempfile::tempdir().unwrap();
    let record = probe.path().join("attempts").to_string_lossy().into_owned();
    let fixture = Fixture::new(json!({
        "mode": "starting", "attempts": record, "failures": 1, "exit_code": 7,
    }));
    let timeout = Duration::from_secs(2);
    let started = Instant::now();
    let error = fixture
        .startup()
        .timeout(timeout)
        .start(&CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.is_deadline_exceeded(), "startup deadline: {error}");
    assert!(
        started.elapsed() <= timeout + Duration::from_secs(2),
        "retry renewed the startup deadline"
    );
    // A slow launch can consume the deadline before the retry or the second
    // child; check cleanup for the children that actually ran.
    if let Ok(payload) = std::fs::read_to_string(&record)
        && !payload.trim().is_empty()
    {
        assert_retired_attempts(
            &record,
            payload
                .lines()
                .filter(|line| !line.trim().is_empty())
                .count(),
        );
    }
}

async fn startup_enables_cloudflared_protocol_fallback() {
    let fixture = Fixture::new(json!({"mode": "tcp-only"}));
    let process = fixture
        .startup()
        .timeout(Duration::from_secs(2))
        .start(&CancellationToken::new())
        .await
        .unwrap_or_else(|error| panic!("tunnel with TCP available: {error}"));
    process.close().await.unwrap();
}

async fn close_keeps_an_observed_failure_while_logs_drain() {
    let probe = tempfile::tempdir().unwrap();
    let exit = probe.path().join("exit");
    let fixture = Fixture::new(json!({"mode": "fail-after-ready", "exit": exit.to_string_lossy()}));
    let entered = CancellationToken::new();
    let exited = CancellationToken::new();
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let resume = {
        let release = Arc::clone(&release);
        move || {
            if let Ok(mut released) = release.0.lock() {
                *released = true;
            }
            release.1.notify_all();
        }
    };
    set_hook(Hook::Block {
        value: "fixture exit failure",
        entered: entered.clone(),
        release: Arc::clone(&release),
        exited: exited.clone(),
    });
    let process = fixture.startup().start(&CancellationToken::new()).await;
    let process = match process {
        Ok(process) => process,
        Err(error) => {
            resume();
            panic!("{error}");
        }
    };
    let observed = async {
        tokio::time::timeout(Duration::from_secs(2), entered.cancelled())
            .await
            .map_err(|_| "fixture did not enter its log writer")?;
        std::fs::write(&exit, b"").map_err(|_| "exit marker")?;
        tokio::time::timeout(Duration::from_secs(2), exited.cancelled())
            .await
            .map_err(|_| "dependency exit was not recorded before draining its logs")
    }
    .await;
    if let Err(message) = observed {
        resume();
        panic!("{message}");
    }
    // The owner closes while the output is still draining; the observed
    // dependency failure must survive the late cancellation.
    let closing = tokio::spawn(async move { process.close().await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    resume();
    let result = tokio::time::timeout(Duration::from_secs(2), closing)
        .await
        .expect("tunnel did not retire after its logs drained")
        .expect("close task");
    let error: Error =
        result.expect_err("late owner cancellation lost the observed dependency failure");
    assert_eq!(error.exit_code(), Some(7), "{error}");
    assert!(
        error.to_string().contains("fixture exit failure"),
        "late output lost the dependency detail: {error}"
    );
}

async fn ready_tunnel_waits_for_its_explicit_ordered_close() {
    let fixture = Fixture::new(json!({"mode": "ready"}));
    let stop = CancellationToken::new();
    let process = fixture.startup().start(&stop).await.unwrap();
    stop.cancel();
    let early = tokio::time::timeout(Duration::from_millis(150), process.done()).await;
    assert!(
        early.is_err(),
        "parent cancellation killed the ready tunnel before its owner closed it"
    );
    process.close().await.unwrap();
    assert!(
        process.is_done(),
        "close returned with the tunnel still running"
    );
    process.close().await.expect("repeat close");
}

async fn startup_cancellation_still_retires_the_tunnel() {
    let fixture = Fixture::new(json!({"mode": "starting"}));
    let started = Instant::now();
    let error = fixture
        .startup()
        .timeout(Duration::from_millis(150))
        .start(&CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.is_deadline_exceeded(), "cancelled startup: {error}");
    assert!(
        started.elapsed() <= Duration::from_secs(2),
        "startup cancellation did not join its child promptly"
    );
}

/// Dropping a ready tunnel without an orderly close still retires the child.
/// Windows holds the image open while it runs, so a writable executable
/// shows that the child has exited.
async fn dropped_tunnel_is_retired() {
    let fixture = Fixture::new(json!({"mode": "ready"}));
    let process = fixture
        .startup()
        .start(&CancellationToken::new())
        .await
        .unwrap();
    drop(process);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let probe = std::fs::OpenOptions::new()
            .write(true)
            .open(&fixture.executable);
        if probe.is_ok() {
            break;
        }
        assert!(Instant::now() < deadline, "dropped tunnel kept running");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
