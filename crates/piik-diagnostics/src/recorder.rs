//! Bounded log files and opt-in local diagnostic reports.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tracing::Level;
use tracing_subscriber::fmt::MakeWriter;
use zip::write::SimpleFileOptions;

use crate::layer::{RedactingLayer, format_record};
use crate::redact::{safe_text, safe_value};

pub(crate) const MAX_FILE_BYTES: u64 = 8 << 20;

/// Recorder failures. Cloneable so the first log write failure can be retained
/// and reported by both [`Recorder::export`] and [`Recorder::close`].
#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    #[error("diagnostics recorder is closed")]
    Closed,
    #[error("{0}")]
    Invalid(String),
    #[error("{context}: {source}")]
    Io {
        context: &'static str,
        source: Arc<io::Error>,
    },
}

impl Error {
    fn io(context: &'static str) -> impl FnOnce(io::Error) -> Self {
        move |source| Self::Io {
            context,
            source: Arc::new(source),
        }
    }
}

type Result<T> = std::result::Result<T, Error>;

/// Owns one component's rotating log (current file plus one backup) and its
/// diagnostic exports. Cloning shares the same recorder.
#[derive(Clone)]
pub struct Recorder {
    shared: Arc<Shared>,
}

struct Shared {
    export: Mutex<ExportState>,
    log: Mutex<RotatingLog>,
    component: &'static str,
    revision: String,
    started: SystemTime,
    started_instant: Instant,
}

#[derive(Default)]
struct ExportState {
    context: Map<String, Value>,
    closed: bool,
    export_path: Option<PathBuf>,
    #[cfg(test)]
    barrier: Option<Box<dyn FnMut() + Send>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Recorder {
    /// Appends to the component's current log, retaining one rotated backup.
    /// Callers own the directory choice.
    pub fn open(directory: impl AsRef<Path>, component: &str, revision: &str) -> Result<Self> {
        let component = match component {
            "client" => "client",
            "server" => "server",
            _ => {
                return Err(Error::Invalid(
                    "diagnostics component must be client or server".into(),
                ));
            }
        };
        let directory = directory.as_ref();
        if directory.as_os_str().is_empty() {
            return Err(Error::Invalid("diagnostics directory is empty".into()));
        }
        let directory =
            std::path::absolute(directory).map_err(Error::io("resolve diagnostics directory"))?;
        fs::create_dir_all(&directory).map_err(Error::io("create diagnostics directory"))?;
        let path = directory.join(format!("{component}.log"));
        let backup = backup_path(&path);
        check_log_file(&path)?;
        check_log_file(&backup)?;
        let file = open_append(&path).map_err(Error::io("open diagnostics log"))?;
        let size = file
            .metadata()
            .map_err(Error::io("open diagnostics log"))?
            .len();
        let log = RotatingLog {
            preexisting: size > 0 || backup.exists(),
            file: Some(file),
            path,
            size,
            err: None,
            rotations: 0,
            closed: false,
        };
        Ok(Self {
            shared: Arc::new(Shared {
                export: Mutex::new(ExportState::default()),
                log: Mutex::new(log),
                component,
                revision: revision.to_owned(),
                started: SystemTime::now(),
                started_instant: Instant::now(),
            }),
        })
    }

    /// Records application and dependency diagnostics at DEBUG and above.
    pub fn layer(&self) -> RedactingLayer<RecorderWriter> {
        RedactingLayer::new(RecorderWriter(self.shared.clone()))
    }

    pub fn log_path(&self) -> PathBuf {
        lock(&self.shared.log).path.clone()
    }

    /// The latest successfully completed archive, including after close.
    pub fn last_export_path(&self) -> Option<PathBuf> {
        lock(&self.shared.export).export_path.clone()
    }

    /// Snapshots an explicitly selected startup fact; it never reads environment files.
    pub fn context(&self, name: &str, value: impl Serialize) {
        let value = serde_json::to_value(value)
            .map(|value| safe_value(name, value))
            .unwrap_or_else(|_| Value::String("[diagnostic value unavailable]".into()));
        lock(&self.shared.export)
            .context
            .insert(name.to_owned(), value);
    }

    /// Records only the identity of an explicitly selected runtime executable.
    pub fn binary(&self, name: &str, path: impl AsRef<Path>) {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            return;
        }
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut identity = json!({ "name": file_name });
        match hash_file(path) {
            Ok((bytes, sha256)) => {
                identity["bytes"] = bytes.into();
                identity["sha256"] = sha256.into();
            }
            Err(error) => identity["error"] = safe_text(&error.to_string()).into(),
        }
        self.context(name, identity);
    }

    /// Retains useful evidence even when an optional collector fails. It never
    /// collects raw process memory, media or arbitrary directory contents.
    pub fn export(&self) -> Result<PathBuf> {
        let mut state = lock(&self.shared.export);
        if state.closed {
            return Err(Error::Closed);
        }
        let now = SystemTime::now();
        let since_epoch = now.duration_since(UNIX_EPOCH).unwrap_or_default();
        let report_id = format!("{}-{}", std::process::id(), since_epoch.as_nanos());
        let record = format_record(
            Level::DEBUG,
            "diagnostic-report",
            &[("reportId".to_owned(), Value::String(report_id.clone()))],
        );
        // Like other log writes, a failure here is retained and surfaces below.
        let _ = lock(&self.shared.log).write(&record);
        let (logs, log_error) = lock(&self.shared.log).snapshot();
        let mut collector_errors = Vec::new();
        if let Some(error) = log_error {
            collector_errors.push(safe_text(&error));
        }
        let directory = self
            .log_path()
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let stamp: String = humantime::format_rfc3339_seconds(now)
            .to_string()
            .chars()
            .filter(|c| *c != '-' && *c != ':')
            .collect();
        let prefix = format!("{}-diagnostics-{stamp}-", self.shared.component);
        // Dropping an unfinished temporary file deletes it.
        let mut temporary = tempfile::Builder::new()
            .prefix(&prefix)
            .suffix(".zip")
            .tempfile_in(&directory)
            .map_err(Error::io("create diagnostics archive"))?;
        {
            let mut archive = Archive {
                writer: zip::ZipWriter::new(temporary.as_file_mut()),
                files: Vec::new(),
            };
            for (name, data) in &logs {
                archive.add(name, data)?;
            }
            #[cfg(test)]
            if let Some(barrier) = state.barrier.as_mut() {
                barrier();
            }
            let context = serde_json::to_vec_pretty(&state.context).expect("JSON values serialize");
            archive.add("context.json", &context)?;
            match memory_stats::memory_stats() {
                Some(memory) => {
                    let memory = json!({ "physicalBytes": memory.physical_mem, "virtualBytes": memory.virtual_mem });
                    archive.add(
                        "memory.json",
                        &serde_json::to_vec_pretty(&memory).expect("JSON values serialize"),
                    )?;
                }
                None => collector_errors
                    .push("memory.json: process memory counters are unavailable".into()),
            }
            archive.add("README.txt", README.as_bytes())?;
            let retention = lock(&self.shared.log).retention();
            let partial =
                !collector_errors.is_empty() || retention["earlierHistoryMayBeMissing"] == true;
            let mut files = archive.files.clone();
            files.push("metadata.json".into());
            let metadata = json!({
                "component": self.shared.component,
                "revision": self.shared.revision,
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "crateVersion": env!("CARGO_PKG_VERSION"),
                "collectedAt": humantime::format_rfc3339_nanos(now).to_string(),
                "startedAt": humantime::format_rfc3339_nanos(self.shared.started).to_string(),
                "uptimeSeconds": self.shared.started_instant.elapsed().as_secs_f64(),
                "reportId": report_id,
                "pid": std::process::id(),
                "cpuCount": std::thread::available_parallelism().map(usize::from).unwrap_or(0),
                "files": files,
                "collectorErrors": collector_errors,
                "partial": partial,
                "logRetention": retention,
            });
            let metadata = serde_json::to_vec_pretty(&metadata).expect("JSON values serialize");
            archive.add("metadata.json", &metadata)?;
            archive.writer.finish().map_err(archive_error)?;
        }
        temporary
            .as_file()
            .sync_all()
            .map_err(Error::io("write diagnostics archive"))?;
        let (_, path) = temporary.keep().map_err(|error| Error::Io {
            context: "keep diagnostics archive",
            source: Arc::new(error.error),
        })?;
        state.export_path = Some(path.clone());
        Ok(path)
    }

    /// Waits for an export already in progress, then closes the log and returns
    /// the first log write failure. Safe to call repeatedly.
    pub fn close(&self) -> Result<()> {
        let mut state = lock(&self.shared.export);
        state.closed = true;
        let mut log = lock(&self.shared.log);
        log.closed = true;
        if let Some(file) = log.file.take()
            && let Err(error) = file.sync_all()
            && log.err.is_none()
        {
            log.err = Some(Error::io("close diagnostics log")(error));
        }
        log.err.clone().map_or(Ok(()), Err)
    }
}

fn archive_error(error: zip::result::ZipError) -> Error {
    Error::Io {
        context: "write diagnostics archive",
        source: Arc::new(error.into()),
    }
}

/// Zip writer that remembers the names of the entries it wrote.
struct Archive<W: Write + io::Seek> {
    writer: zip::ZipWriter<W>,
    files: Vec<String>,
}

impl<W: Write + io::Seek> Archive<W> {
    fn add(&mut self, name: &str, data: &[u8]) -> Result<()> {
        self.writer
            .start_file(name, SimpleFileOptions::default())
            .map_err(archive_error)?;
        self.writer
            .write_all(data)
            .map_err(Error::io("write diagnostics archive"))?;
        self.files.push(name.to_owned());
        Ok(())
    }
}

const README: &str = "Piik diagnostic report / Piik 诊断报告\n\n\
Local opt-in report. Review before sharing: technical identifiers, paths and network/system details may remain.\n\
Credentials are filtered; raw media and process-memory contents are not collected. memory.json holds process counters only.\n\
metadata.json lists retained files and collector failures. Rotated logs are a bounded history, not the entire session.\n\n\
本报告由本机主动导出。分享前请检查：可能含技术标识、路径和网络/系统信息。\n\
凭据已过滤；不采集原始媒体或进程内存内容。memory.json 只含进程计数。\n\
metadata.json 记录文件及采集失败；轮转日志只保留有限历史。\n";

/// [`MakeWriter`] for a recorder's rotating log; each event is one write.
pub struct RecorderWriter(Arc<Shared>);

impl<'a> MakeWriter<'a> for RecorderWriter {
    type Writer = RecordWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        RecordWriter(&self.0)
    }
}

pub struct RecordWriter<'a>(&'a Shared);

impl Write for RecordWriter<'_> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        lock(&self.0.log)
            .write(data)
            .map(|()| data.len())
            .map_err(io::Error::other)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) struct RotatingLog {
    pub(crate) file: Option<File>,
    pub(crate) path: PathBuf,
    size: u64,
    err: Option<Error>,
    rotations: u64,
    preexisting: bool,
    closed: bool,
}

impl RotatingLog {
    /// Writes one whole record. Layers discard sink errors, so the first
    /// failure is retained for export and close.
    pub(crate) fn write(&mut self, data: &[u8]) -> Result<()> {
        if self.closed {
            return Err(Error::Closed);
        }
        let result = self.write_record(data);
        if let Err(error) = &result
            && self.err.is_none()
        {
            self.err = Some(error.clone());
        }
        result
    }

    fn write_record(&mut self, data: &[u8]) -> Result<()> {
        let length = data.len() as u64;
        if length > MAX_FILE_BYTES {
            return Err(Error::Invalid(
                "diagnostics log record exceeds 8 MiB".into(),
            ));
        }
        if self.file.is_some() && self.size + length > MAX_FILE_BYTES {
            self.rotate()?;
        }
        let file = self.file.as_mut().ok_or(Error::Closed)?;
        file.write_all(data)
            .map_err(Error::io("write diagnostics log"))?;
        self.size += length;
        Ok(())
    }

    fn rotate(&mut self) -> Result<()> {
        drop(self.file.take());
        let backup = backup_path(&self.path);
        match fs::remove_file(&backup) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => {
                return Err(Error::io("rotate diagnostics log")(error));
            }
            _ => {}
        }
        fs::rename(&self.path, &backup).map_err(Error::io("rotate diagnostics log"))?;
        self.file = Some(open_append(&self.path).map_err(Error::io("rotate diagnostics log"))?);
        self.size = 0;
        self.rotations += 1;
        Ok(())
    }

    pub(crate) fn retention(&self) -> Value {
        json!({
            "fileBytesLimit": MAX_FILE_BYTES,
            "fileCountLimit": 2,
            "rotationsThisProcess": self.rotations,
            "preexistingHistory": self.preexisting,
            "earlierHistoryMayBeMissing": self.preexisting || self.rotations > 0,
        })
    }

    /// Copies at most 16 MiB under the log lock; compression runs outside it.
    fn snapshot(&mut self) -> (Vec<(String, Vec<u8>)>, Option<String>) {
        let mut issues: Vec<String> = self.err.iter().map(ToString::to_string).collect();
        if let Some(file) = &self.file
            && let Err(error) = file.sync_all()
        {
            issues.push(error.to_string());
        }
        let mut logs = Vec::new();
        for path in [backup_path(&self.path), self.path.clone()] {
            if let Err(error) = check_log_file(&path) {
                issues.push(error.to_string());
                continue;
            }
            let file = match File::open(&path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound && path != self.path => {
                    continue;
                }
                Err(error) => {
                    issues.push(error.to_string());
                    continue;
                }
                Ok(file) => file,
            };
            let mut data = Vec::new();
            if let Err(error) = file.take(MAX_FILE_BYTES + 1).read_to_end(&mut data) {
                issues.push(error.to_string());
                continue;
            }
            if data.len() as u64 > MAX_FILE_BYTES {
                issues.push("diagnostics log exceeds 8 MiB".into());
                continue;
            }
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            logs.push((name, data));
        }
        let issues = (!issues.is_empty()).then(|| issues.join("\n"));
        (logs, issues)
    }
}

fn backup_path(path: &Path) -> PathBuf {
    let mut backup = path.as_os_str().to_owned();
    backup.push(".1");
    backup.into()
}

fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

fn check_log_file(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::io("inspect diagnostics log")(error)),
        Ok(info) if !info.is_file() || info.len() > MAX_FILE_BYTES => Err(Error::Invalid(format!(
            "diagnostics log {path:?} must be a regular file no larger than 8 MiB"
        ))),
        Ok(_) => Ok(()),
    }
}

fn hash_file(path: &Path) -> io::Result<(u64, String)> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 64 << 10];
    let mut size = 0;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            return Ok((size, hex::encode(hash.finalize())));
        }
        hash.update(&buffer[..count]);
        size += count as u64;
    }
}

#[cfg(test)]
impl Recorder {
    pub(crate) fn with_log<T>(&self, change: impl FnOnce(&mut RotatingLog) -> T) -> T {
        change(&mut lock(&self.shared.log))
    }

    pub(crate) fn set_barrier(&self, barrier: impl FnMut() + Send + 'static) {
        lock(&self.shared.export).barrier = Some(Box::new(barrier));
    }
}
