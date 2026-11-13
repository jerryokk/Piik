//! Redacted structured logging on `tracing`, bounded log files, and opt-in
//! local diagnostic reports.
//!
//! Every record written through [`RedactingLayer`] (including the
//! [`Recorder`]'s) applies one secret policy: private field names are replaced,
//! identity fields are hashed with [`id`], and all text passes [`safe_text`].

mod layer;
mod recorder;
mod redact;

pub use layer::{Json, LineWriter, RedactingLayer};
pub use recorder::{Error, RecordWriter, Recorder, RecorderWriter};
pub use redact::{id, safe_text, safe_value};

#[cfg(test)]
mod tests;
