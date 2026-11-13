//! Committed text: one explicit submission. Newlines and tabs use real keys;
//! Unicode events handle the remaining text independently of layout.

use crate::Error;
use crate::event::{Bounds, InputEvent};

pub(crate) const MAX_TEXT_BYTES: usize = 2048;
/// JSON escaping and grant fields.
pub(crate) const MAX_INPUT_BYTES: usize = MAX_TEXT_BYTES * 2 + 256;

pub(crate) trait TextSink {
    fn reset(&mut self) -> Result<(), Error>;
    fn apply(&mut self, target: Bounds, event: InputEvent) -> Result<(), Error>;
}

/// `text` is already valid UTF-8; the bound counts its UTF-8 bytes.
pub(crate) fn apply_text(sink: &mut dyn TextSink, target: Bounds, text: &str) -> Result<(), Error> {
    if text.is_empty() || text.len() > MAX_TEXT_BYTES {
        return Err(Error::input("invalid input text"));
    }
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    if text
        .chars()
        .any(|c| (c < '\u{20}' && c != '\n' && c != '\t') || c == '\u{7f}')
    {
        return Err(Error::input("text contains a control character"));
    }
    sink.reset()?;
    let mut rest = text.as_str();
    while !rest.is_empty() {
        let Some(end) = rest.find(['\n', '\t']) else {
            return sink.apply(target, InputEvent::text(rest));
        };
        if end > 0 {
            sink.apply(target, InputEvent::text(&rest[..end]))?;
        }
        let code = if rest.as_bytes()[end] == b'\t' {
            "Tab"
        } else {
            "Enter"
        };
        for down in [true, false] {
            sink.apply(target, InputEvent::key(code, down))?;
        }
        rest = &rest[end + 1..];
    }
    Ok(())
}
