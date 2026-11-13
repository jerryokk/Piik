//! Redacting JSON-lines `tracing` layer and dependency line capture.

use std::fmt::{self, Write as _};
use std::io::{self, Write};

use serde::Serialize;
use serde_json::Value;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

use crate::redact::{identity_field, private_field, safe_text, safe_value};

/// Bound for one dependency stderr line; longer input is counted, not kept.
const MAX_LINE_BYTES: usize = 64 << 10;
/// Marks a [`Json`] field so the layer records structure instead of text.
const JSON_MARKER: &str = "\u{1e}piik-json\u{1e}";
const DEPENDENCY_TARGET: &str = "piik_diagnostics::dependency";

/// Records a serializable value as a structured, redacted JSON field:
/// `tracing::info!(nested = ?Json(&value), "...")`.
pub struct Json<T>(pub T);

impl<T: Serialize> fmt::Debug for Json<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match serde_json::to_string(&self.0) {
            Ok(text) => write!(formatter, "{JSON_MARKER}{text}"),
            Err(_) => formatter.write_str("[diagnostic value unavailable]"),
        }
    }
}

/// Ordinary console sinks keep only Piik's own events at INFO or above.
/// Dependency records belong to the opt-in [`Recorder`](crate::Recorder) layer.
fn console_event(metadata: &Metadata<'_>) -> bool {
    application_target(metadata.target())
        && metadata.target() != DEPENDENCY_TARGET
        && *metadata.level() <= Level::INFO
}

fn application_target(target: &str) -> bool {
    target == "piik" || target.starts_with("piik_") || target.starts_with("piik::")
}

/// Formats every event as one redacted JSON line (`time`, `level`, `msg`, then
/// span and event fields) and writes it with a single write call.
pub struct RedactingLayer<W> {
    writer: W,
    console: bool,
}

impl<W> RedactingLayer<W> {
    /// Records DEBUG and higher events; per-packet TRACE is not collected.
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            console: false,
        }
    }

    /// Records only Piik's own INFO-or-higher events. The check runs at event
    /// delivery: as a per-layer `with_filter`, a bare `tracing::enabled!`
    /// probe left filter state behind that dropped the next event.
    pub fn console(writer: W) -> Self {
        Self {
            writer,
            console: true,
        }
    }
}

struct SpanFields(Vec<(String, Value)>);

impl<S, W> tracing_subscriber::Layer<S> for RedactingLayer<W>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    W: for<'a> MakeWriter<'a> + 'static,
{
    // Reject TRACE before formatting: str0m traces packet payloads on the
    // media thread, which can overwhelm both forwarding and log retention.
    fn enabled(&self, metadata: &Metadata<'_>, _: Context<'_, S>) -> bool {
        *metadata.level() <= Level::DEBUG
    }

    fn max_level_hint(&self) -> Option<tracing_subscriber::filter::LevelFilter> {
        Some(tracing_subscriber::filter::LevelFilter::DEBUG)
    }

    fn on_new_span(&self, attributes: &Attributes<'_>, id: &Id, context: Context<'_, S>) {
        let Some(span) = context.span(id) else { return };
        let mut visitor = FieldVisitor::default();
        attributes.record(&mut visitor);
        span.extensions_mut().insert(SpanFields(visitor.fields));
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, context: Context<'_, S>) {
        let Some(span) = context.span(id) else { return };
        let mut visitor = FieldVisitor::default();
        values.record(&mut visitor);
        let mut extensions = span.extensions_mut();
        if let Some(fields) = extensions.get_mut::<SpanFields>() {
            for (key, value) in visitor.fields {
                fields.0.retain(|(existing, _)| *existing != key);
                fields.0.push((key, value));
            }
        }
    }

    fn on_event(&self, event: &Event<'_>, context: Context<'_, S>) {
        if self.console && !console_event(event.metadata()) {
            return;
        }
        let mut fields = Vec::new();
        if let Some(scope) = context.event_scope(event) {
            for span in scope.from_root() {
                if let Some(span_fields) = span.extensions().get::<SpanFields>() {
                    fields.extend(span_fields.0.iter().cloned());
                }
            }
        }
        let target = event.metadata().target();
        if !application_target(target) {
            fields.push(("library".to_owned(), Value::String(target.to_owned())));
        }
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        fields.extend(visitor.fields);
        let line = format_record(*event.metadata().level(), &visitor.message, &fields);
        // Sinks own their failures; the recorder retains the first one.
        let _ = self
            .writer
            .make_writer_for(event.metadata())
            .write_all(&line);
    }
}

pub(crate) fn format_record(level: Level, message: &str, fields: &[(String, Value)]) -> Vec<u8> {
    let level = match level {
        Level::ERROR => "ERROR",
        Level::WARN => "WARN",
        Level::INFO => "INFO",
        _ => "DEBUG",
    };
    let time = humantime::format_rfc3339_nanos(std::time::SystemTime::now()).to_string();
    let mut line = String::with_capacity(128);
    let _ = write!(
        line,
        "{{\"time\":{},\"level\":\"{level}\",\"msg\":{}",
        Value::String(time),
        Value::String(safe_text(message))
    );
    for (key, value) in fields {
        let _ = write!(line, ",{}:{value}", Value::String(key.clone()));
    }
    line.push_str("}\n");
    line.into_bytes()
}

#[derive(Default)]
struct FieldVisitor {
    message: String,
    fields: Vec<(String, Value)>,
}

impl FieldVisitor {
    fn push(&mut self, field: &Field, value: Value) {
        let key = field.name();
        let value = if private_field(key) {
            Value::String("[redacted]".to_owned())
        } else {
            value
        };
        self.fields.push((key.to_owned(), value));
    }

    fn text(&mut self, field: &Field, text: &str) {
        if field.name() == "message" {
            self.message = safe_text(text);
        } else if let Some(json) = text.strip_prefix(JSON_MARKER) {
            let value = serde_json::from_str(json).unwrap_or(Value::Null);
            self.push(field, safe_value(field.name(), value));
        } else if identity_field(field.name()) {
            self.push(
                field,
                safe_value(field.name(), Value::String(text.to_owned())),
            );
        } else {
            self.push(field, Value::String(safe_text(text)));
        }
    }
}

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.text(field, &format!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.text(field, value);
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.push(field, value.into());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.push(field, value.into());
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.push(field, value.into());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.push(field, value.into());
    }

    /// Records `{message, systemCode}`: the full cause chain and the first OS
    /// error code found in it.
    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        let mut message = value.to_string();
        let mut code = None;
        let mut cause = Some(value);
        while let Some(error) = cause {
            if code.is_none() {
                code = error
                    .downcast_ref::<io::Error>()
                    .and_then(io::Error::raw_os_error);
            }
            cause = error.source();
            if let Some(next) = cause {
                let _ = write!(message, ": {next}");
            }
        }
        let mut object = serde_json::Map::new();
        object.insert("message".to_owned(), Value::String(safe_text(&message)));
        if let Some(code) = code {
            object.insert("systemCode".to_owned(), code.into());
        }
        self.push(field, Value::Object(object));
    }
}

/// Turns dependency stderr into bounded DEBUG records with `library = scope`.
/// Incomplete trailing input is emitted by [`LineWriter::close`] or on drop.
pub struct LineWriter {
    scope: String,
    line: Vec<u8>,
    omitted: usize,
    private_key: bool,
}

impl LineWriter {
    pub fn new(scope: impl Into<String>) -> Self {
        Self {
            scope: scope.into(),
            line: Vec::new(),
            omitted: 0,
            private_key: false,
        }
    }

    /// Emits any pending partial line.
    pub fn close(&mut self) {
        self.emit();
    }

    fn emit(&mut self) {
        let text = String::from_utf8_lossy(&self.line);
        let mut line = text.strip_suffix('\r').unwrap_or(&text).to_owned();
        if line.contains("-----BEGIN ") && line.contains("PRIVATE KEY-----") {
            self.private_key = true;
        }
        if self.private_key {
            self.private_key = !(line.contains("-----END ") && line.contains("PRIVATE KEY-----"));
            line = "[private key redacted]".to_owned();
        }
        if !line.is_empty() || self.omitted > 0 {
            tracing::debug!(
                target: DEPENDENCY_TARGET,
                library = %self.scope,
                line = %safe_text(&line),
                omittedBytes = self.omitted,
                "dependency"
            );
        }
        self.line.clear();
        self.omitted = 0;
    }
}

impl Write for LineWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        for &byte in data {
            if byte == b'\n' {
                self.emit();
            } else if self.line.len() < MAX_LINE_BYTES {
                self.line.push(byte);
            } else {
                self.omitted += 1;
            }
        }
        Ok(data.len())
    }

    /// Lines are emitted at each newline; flushing never splits a line.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for LineWriter {
    fn drop(&mut self) {
        self.emit();
    }
}
