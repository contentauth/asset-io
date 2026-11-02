// Copyright 2026 Adobe. All rights reserved.
// This file is licensed to you under the Apache License,
// Version 2.0 (http://www.apache.org/licenses/LICENSE-2.0)
// or the MIT license (http://opensource.org/licenses/MIT),
// at your option.

//! Parsing orchestration: select a handler, then run its parser.

use crate::{block_on, Error, HandlerRegistry, Io, ReadSeek, Result, Structure, Task, DETECT_LEN};
use std::io::SeekFrom;
use std::sync::Arc;

/// Parse a complete stream with blocking I/O.
///
/// `Some(format)` selects a handler by extension or MIME type; `None` detects
/// the container from its leading bytes. An unsupported explicit format fails
/// without falling back to detection. Format selection does not validate the
/// content; the selected handler's parser does that.
///
/// The stream's length is determined by seeking to its end. Offsets are relative
/// to the beginning of the stream, regardless of its initial position. The
/// stream may be repositioned on success or error.
pub fn parse(
    registry: &HandlerRegistry,
    src: &mut dyn ReadSeek,
    format: Option<&str>,
) -> Result<Structure> {
    let total = src.seek(SeekFrom::End(0))?;
    block_on(total, src, |mut io| async move {
        parse_async(registry, &mut io, format).await
    })
}

/// Parse a stream of `total` bytes as a caller-driven task.
///
/// Format selection follows [`parse`]. Selection and parsing errors are returned
/// when the task is driven, not when it is created. The task borrows `registry`
/// and `format`; to build a registry-independent task for a known handler, use
/// [`Task::new`] with [`ContainerHandler::parse_async`](crate::ContainerHandler::parse_async).
pub fn parse_task<'a>(
    registry: &'a HandlerRegistry,
    total: u64,
    format: Option<&'a str>,
) -> Task<'a, Structure> {
    Task::new(total, move |mut io| async move {
        parse_async(registry, &mut io, format).await
    })
}

/// Parse through `io` inside a larger sans-I/O computation.
///
/// Format selection follows [`parse`]. This does no I/O itself: a [`Task`] driver
/// supplies requested bytes. The selected handler takes an owned [`Io::fork`],
/// leaving `io` available after parsing. The fork has a separate byte window, so
/// detection bytes may be requested again by the parser.
pub async fn parse_async(
    registry: &HandlerRegistry,
    io: &mut Io,
    format: Option<&str>,
) -> Result<Structure> {
    let handler = match format {
        Some(format) => Arc::clone(registry.handler(format).ok_or(Error::UnsupportedFormat)?),
        None => {
            let len = (DETECT_LEN as u64).min(io.len()) as usize;
            let head = io.fetch(0, len).await?;
            Arc::clone(registry.detect(head).ok_or(Error::UnsupportedFormat)?)
        }
    };
    handler.parse_async(io.fork()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BoxFuture, ContainerHandler, FormatSpec, Name, Progress, SegmentKind, StructureBuilder,
    };
    use std::io::{Cursor, Read, Seek};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct TestHandler {
        id: &'static str,
        detects: bool,
        detections: Arc<AtomicUsize>,
    }

    impl ContainerHandler for TestHandler {
        fn id(&self) -> &'static str {
            self.id
        }

        fn formats(&self) -> &[FormatSpec] {
            const FORMATS: &[FormatSpec] = &[FormatSpec::new("application/x-test", "test")];
            FORMATS
        }

        fn detect(&self, header: &[u8]) -> bool {
            self.detections.fetch_add(1, Ordering::Relaxed);
            self.detects && header.starts_with(b"TEST")
        }

        fn parse_async(self: Arc<Self>, mut io: Io) -> BoxFuture<'static, Result<Structure>> {
            Box::pin(async move {
                let mut head = [0; 4];
                io.read_at(0, &mut head).await?;
                if head != *b"TEST" {
                    return Err(Error::InvalidFormat("expected TEST header".into()));
                }
                let mut builder = StructureBuilder::new(self, "application/x-test", io.len());
                builder.push_span(
                    SegmentKind::Other,
                    Name::FourCC(*b"TEST"),
                    4,
                    io.len() - 4,
                    0,
                )?;
                builder.finish()
            })
        }
    }

    fn handler(id: &'static str, detects: bool) -> TestHandler {
        TestHandler {
            id,
            detects,
            detections: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn drive<T>(mut task: Task<'_, T>, bytes: &[u8]) -> (Result<T>, usize) {
        let mut requests = 0;
        loop {
            match task.resume() {
                Ok(Progress::Read(mut req)) => {
                    requests += 1;
                    let start = (req.offset() as usize).min(bytes.len());
                    let end = (start + req.window_len()).min(bytes.len());
                    req.buffer().extend_from_slice(&bytes[start..end]);
                }
                Ok(Progress::Done(value)) => return (Ok(value), requests),
                Err(error) => return (Err(error), requests),
            }
        }
    }

    #[test]
    fn explicit_formats_skip_detection_and_support_handlers_without_detectors() {
        let h = handler("explicit", false);
        let detections = Arc::clone(&h.detections);
        let registry = HandlerRegistry::new().with_handler(h);
        for format in [" .TeSt ", "APPLICATION/X-TEST"] {
            let mut src = Cursor::new(b"TESTpayload");
            src.set_position(5);
            let structure = parse(&registry, &mut src, Some(format)).unwrap();
            assert_eq!(structure.container_id(), "explicit");
            assert_eq!(structure.total_size(), 11);
            let (structure, _) = drive(parse_task(&registry, 11, Some(format)), b"TESTpayload");
            assert_eq!(structure.unwrap().container_id(), "explicit");
        }
        assert_eq!(detections.load(Ordering::Relaxed), 0);
        assert!(matches!(
            parse(&registry, &mut Cursor::new(b"TEST"), None),
            Err(Error::UnsupportedFormat)
        ));
    }

    #[test]
    fn detection_preserves_registration_order_and_fallback_precedence() {
        let base = Arc::new(HandlerRegistry::new().with_handler(handler("fallback", true)));
        let mut registry = HandlerRegistry::with_fallback(Arc::clone(&base));
        registry.add_handler(handler("first", true));
        registry.add_handler(handler("latest", true));
        for format in [None, Some("test")] {
            let structure = parse(&registry, &mut Cursor::new(b"TEST"), format).unwrap();
            assert_eq!(structure.container_id(), "latest");
            let (structure, _) = drive(parse_task(&registry, 4, format), b"TEST");
            assert_eq!(structure.unwrap().container_id(), "latest");
        }
        let registry = HandlerRegistry::with_fallback(base);
        for format in [None, Some("test")] {
            assert_eq!(
                parse(&registry, &mut Cursor::new(b"TEST"), format)
                    .unwrap()
                    .container_id(),
                "fallback"
            );
        }
    }

    #[test]
    fn unknown_explicit_formats_and_parser_errors_do_not_fall_back() {
        let h = handler("test", true);
        let detections = Arc::clone(&h.detections);
        let registry = HandlerRegistry::new().with_handler(h);
        for format in [Some("unknown"), Some("")] {
            assert!(matches!(
                parse(&registry, &mut Cursor::new(b"TEST"), format),
                Err(Error::UnsupportedFormat)
            ));
            let (result, requests) = drive(parse_task(&registry, 4, format), b"TEST");
            assert!(matches!(result, Err(Error::UnsupportedFormat)));
            assert_eq!(requests, 0);
        }
        assert!(matches!(
            parse(&registry, &mut Cursor::new(b"nope"), Some("test")),
            Err(Error::InvalidFormat(_))
        ));
        assert_eq!(detections.load(Ordering::Relaxed), 0);
        for bytes in [b"".as_slice(), b"T", b"nope"] {
            assert!(matches!(
                parse(&registry, &mut Cursor::new(bytes), None),
                Err(Error::UnsupportedFormat)
            ));
        }
        let (result, _) = drive(parse_task(&registry, 4, Some("test")), b"T");
        assert!(matches!(
            result,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof
        ));
    }

    #[test]
    fn async_parsing_composes_with_later_reads_in_the_same_task() {
        let registry = HandlerRegistry::new().with_handler(handler("test", true));
        for format in [None, Some("test")] {
            let registry = &registry;
            let task = Task::new(11, |mut io| async move {
                let structure = parse_async(registry, &mut io, format).await?;
                let payload = io.read_vec(4, 7).await?;
                Ok((structure, payload))
            });
            let (result, _) = drive(task, b"TESTpayload");
            let (structure, payload) = result.unwrap();
            assert_eq!(structure.container_id(), "test");
            assert_eq!(payload, b"payload");
        }
    }

    #[test]
    fn blocking_parse_propagates_source_errors() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("read failed"))
            }
        }
        impl Seek for Broken {
            fn seek(&mut self, _: SeekFrom) -> std::io::Result<u64> {
                Ok(4)
            }
        }
        let registry = HandlerRegistry::new().with_handler(handler("test", true));
        for format in [None, Some("test")] {
            assert!(matches!(
                parse(&registry, &mut Broken, format),
                Err(Error::Io(error)) if error.to_string() == "read failed"
            ));
        }
    }
}
