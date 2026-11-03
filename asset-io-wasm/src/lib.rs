//! Caller-driven parsing. JavaScript owns I/O and all async scheduling.

use asset_io::{HandlerRegistry, Progress, Structure, Task};
use serde_json::{json, Value};
use wasm_bindgen::prelude::*;

/// An absolute read descriptor. The u64 offset is a JavaScript bigint.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadRequest {
    pub offset: u64,
    pub min_len: usize,
    pub window_len: usize,
}

enum State {
    Ready,
    Read(ReadRequest),
    Done(Structure),
    Failed(String),
}

/// A source-free parser. Calls are synchronous; no JS callback or Rust executor
/// is invoked. Free it to cancel, coordinating outstanding host reads first.
#[wasm_bindgen]
pub struct ParseTask {
    task: Task<'static, Structure>,
    state: State,
}

#[wasm_bindgen]
impl ParseTask {
    /// size is a bigint. Undefined format detects; otherwise select an extension
    /// or MIME type. Input strings and the builtin registry are owned by the task.
    #[wasm_bindgen(constructor)]
    pub fn new(size: u64, format: Option<String>) -> Self {
        let registry = HandlerRegistry::builtin();
        Self {
            task: Task::new(size, move |mut io| async move {
                Structure::parse_async(&registry, &mut io, format.as_deref()).await
            }),
            state: State::Ready,
        }
    }

    /// Return the pending read, or undefined on success. Errors throw and are
    /// terminal. Repeated calls while waiting return the same descriptor.
    pub fn resume(&mut self) -> Result<Option<ReadRequest>, JsError> {
        self.step().map_err(|message| JsError::new(&message))
    }

    /// Copy a Uint8Array response. Oversized responses are rejected without
    /// consuming the request; short responses become EOF errors on resume.
    pub fn supply(&mut self, bytes: &[u8]) -> Result<(), JsError> {
        self.respond(bytes)
            .map_err(|message| JsError::new(&message))
    }

    /// Record a host read failure. Subsequent resume calls throw this message.
    pub fn fail(&mut self, message: String) -> Result<(), JsError> {
        self.fail_read(message)
            .map_err(|message| JsError::new(&message))
    }

    /// Source-free inspection summary, available only after completion.
    /// This is a proof-specific schema, not the complete C ABI inspection schema.
    #[wasm_bindgen(js_name = summaryJson)]
    pub fn summary_json(&self) -> Result<String, JsError> {
        self.summary().map_err(|message| JsError::new(&message))
    }
}

impl ParseTask {
    fn step(&mut self) -> Result<Option<ReadRequest>, String> {
        match &self.state {
            State::Read(request) => return Ok(Some(*request)),
            State::Done(_) => return Ok(None),
            State::Failed(message) => return Err(message.clone()),
            State::Ready => {}
        }
        let state = match self.task.resume() {
            Ok(Progress::Read(req)) => State::Read(ReadRequest {
                offset: req.offset(),
                min_len: req.min_len(),
                window_len: req.window_len(),
            }),
            Ok(Progress::Done(structure)) => State::Done(structure),
            Err(error) => State::Failed(error.to_string()),
            Ok(_) => State::Failed("unsupported task progress".into()),
        };
        self.state = state;
        match &self.state {
            State::Read(request) => Ok(Some(*request)),
            State::Done(_) => Ok(None),
            State::Failed(message) => Err(message.clone()),
            State::Ready => unreachable!(),
        }
    }

    fn respond(&mut self, bytes: &[u8]) -> Result<(), String> {
        let State::Read(request) = &self.state else {
            return Err("task has no outstanding read".into());
        };
        if bytes.len() > request.window_len {
            return Err("response exceeds requested window".into());
        }
        {
            let mut req = self
                .task
                .pending_read()
                .ok_or("missing core read request")?;
            req.buffer().clear();
            req.buffer().extend_from_slice(bytes);
        }
        self.state = State::Ready;
        Ok(())
    }

    fn fail_read(&mut self, message: String) -> Result<(), String> {
        if !matches!(self.state, State::Read(_)) {
            return Err("task has no outstanding read".into());
        }
        self.state = State::Failed(format!("host read failed: {message}"));
        Ok(())
    }

    fn summary(&self) -> Result<String, String> {
        let State::Done(structure) = &self.state else {
            return Err("task has not completed successfully".into());
        };
        Ok(inspection_summary(structure).to_string())
    }
}

fn range(r: asset_io::ByteRange) -> Value {
    json!({"offset": r.offset().to_string(), "length": r.len().to_string()})
}

/// The proof's geometry summary. Decimal strings preserve u64 precision in JS.
/// Shared with the native blocking oracle, not a second parsing implementation.
pub fn inspection_summary(s: &Structure) -> Value {
    json!({
        "schema": 1,
        "container": s.container_id(),
        "media_type": s.media_type(),
        "size": s.total_size().to_string(),
        "trailing": s.trailing().map(range),
        "segments": s.segments().iter().map(|segment| json!({
            "kind": segment.kind().as_str(),
            "range": range(segment.range()),
            "subdivides": segment.subdivides(),
            "regions": segment.regions().map(|(role, bytes)| json!({
                "role": match role {
                    asset_io::Role::Header => "header",
                    asset_io::Role::Payload => "payload",
                    asset_io::Role::Trailer => "trailer",
                },
                "range": range(bytes),
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "items": s.items().map(|item| json!({
            "kind": item.kind().as_str(),
            "encoding": item.encoding().as_str(),
            "size": item.len().to_string(),
            "extents": item.extents().map(|extent| json!({
                "segment": extent.segment().index(),
                "range": range(extent.range()),
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "subassets": s.subassets().iter().map(|asset| json!({
            "range": range(asset.range()),
            "segment_start": asset.segments().start,
            "segment_end": asset.segments().end,
            "media_type": asset.media_type(),
            "primary": asset.is_primary(),
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_ranges_preserve_integer_precision() {
        let offset = (1u64 << 53) + 1;
        let bytes = asset_io::ByteRange::new(offset, 7).unwrap();
        assert_eq!(
            range(bytes),
            json!({"offset": offset.to_string(), "length": "7"})
        );
    }

    #[test]
    fn format_errors_are_deferred_and_terminal() {
        let mut task = ParseTask::new(4, Some("not-a-format".into()));
        assert!(task.respond(b"TEST").is_err());
        assert!(task.summary().is_err());
        let error = task.step().unwrap_err();
        assert_eq!(task.step().unwrap_err(), error);
    }

    #[cfg(feature = "jpeg")]
    #[test]
    fn request_lifecycle_and_result_match_blocking() {
        let bytes = b"\xff\xd8\xff\xd9";
        let mut task = ParseTask::new(4, None);
        let first = task.step().unwrap().unwrap();
        assert_eq!(task.step().unwrap(), Some(first));
        assert!(task.respond(b"12345").is_err());
        assert_eq!(task.step().unwrap(), Some(first));
        while let Some(req) = task.step().unwrap() {
            let at = req.offset as usize;
            task.respond(&bytes[at..at + req.window_len]).unwrap();
        }
        let registry = HandlerRegistry::builtin();
        let structure =
            Structure::parse(&registry, &mut std::io::Cursor::new(bytes), None).unwrap();
        assert_eq!(
            task.summary().unwrap(),
            inspection_summary(&structure).to_string()
        );
        assert!(task.step().unwrap().is_none());
        assert!(task.respond(bytes).is_err());
    }

    #[cfg(feature = "jpeg")]
    #[test]
    fn eof_host_failure_and_cancellation() {
        let mut short = ParseTask::new(4, None);
        short.step().unwrap();
        short.respond(&[]).unwrap();
        let error = short.step().unwrap_err();
        assert_eq!(short.step().unwrap_err(), error);
        let mut failed = ParseTask::new(4, None);
        assert!(failed.fail_read("disk error".into()).is_err());
        failed.step().unwrap();
        failed.fail_read("disk error".into()).unwrap();
        assert!(failed.step().unwrap_err().contains("disk error"));
        assert!(failed.summary().is_err());
        let mut cancelled = ParseTask::new(4, None);
        cancelled.step().unwrap();
        drop(cancelled);
    }
}
