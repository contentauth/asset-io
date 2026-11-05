// Copyright 2026 Adobe. All rights reserved.
// This file is licensed to you under the Apache License,
// Version 2.0 (http://www.apache.org/licenses/LICENSE-2.0)
// or the MIT license (http://opensource.org/licenses/MIT),
// at your option.

//! Parsing: select a handler from a registry, then run its parser to build a
//! [`Structure`].

use std::{io::SeekFrom, sync::Arc};

use crate::{
    container_api::{ReadSeek, DETECT_LEN},
    io::Io,
    task::{block_on, Task},
    Error, HandlerRegistry, Result, Structure,
};

impl Structure {
    /// Parse a complete stream with blocking I/O.
    ///
    /// Content detection takes precedence. If no handler recognizes the content,
    /// `Some(format)` selects a handler by extension or MIME type; with `None`,
    /// parsing fails. Format selection does not validate the content; the selected
    /// handler's parser does that.
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
            Structure::parse_async(registry, &mut io, format).await
        })
    }

    /// Parse a stream of `total` bytes as a caller-driven task.
    ///
    /// Format selection follows [`parse`](Self::parse). Selection and parsing
    /// errors are returned when the task is driven, not when it is created. The
    /// task borrows `registry` and `format`; to build a registry-independent task
    /// for a known handler, use [`Task::new`] with
    /// [`ContainerHandler::parse_async`](crate::container_api::ContainerHandler::parse_async).
    pub fn parse_task<'a>(
        registry: &'a HandlerRegistry,
        total: u64,
        format: Option<&'a str>,
    ) -> Task<'a, Structure> {
        Task::new(total, move |mut io| async move {
            Structure::parse_async(registry, &mut io, format).await
        })
    }

    /// Parse through `io` inside a larger sans-I/O computation.
    ///
    /// Format selection follows [`parse`](Self::parse). This does no I/O itself:
    /// a [`Task`] driver supplies requested bytes. The selected handler takes an
    /// owned [`Io::fork`], leaving `io` available after parsing. The fork has a
    /// separate byte window, so detection bytes may be requested again by the
    /// parser.
    pub async fn parse_async(
        registry: &HandlerRegistry,
        io: &mut Io,
        format: Option<&str>,
    ) -> Result<Structure> {
        let len = (DETECT_LEN as u64).min(io.len()) as usize;
        let head = io.fetch(0, len).await?;
        let detected = registry.detect(head);
        let handler = match (detected, format) {
            (Some(detected), Some(format)) => match registry.handler(format) {
                Some(explicit) if explicit.id() == detected.id() => Arc::clone(explicit),
                _ => Arc::clone(detected),
            },
            (Some(detected), None) => Arc::clone(detected),
            (None, Some(format)) => {
                Arc::clone(registry.handler(format).ok_or(Error::UnsupportedFormat)?)
            }
            (None, None) => return Err(Error::UnsupportedFormat),
        };
        handler.parse_async(io.fork()).await
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Cursor, Read, Seek},
        sync::atomic::{AtomicUsize, Ordering},
    };

    use super::*;
    use crate::{
        container_api::{BoxFuture, ContainerHandler, FormatSpec, StructureBuilder},
        task::Progress,
        Name, SegmentKind,
    };

    #[derive(Debug)]
    struct TestHandler {
        id: &'static str,
        format: &'static str,
        detects: bool,
        detections: Arc<AtomicUsize>,
    }

    impl ContainerHandler for TestHandler {
        fn id(&self) -> &'static str {
            self.id
        }

        fn formats(&self) -> &[FormatSpec] {
            const TEST: &[FormatSpec] = &[FormatSpec::new("application/x-test", "test")];
            const OTHER: &[FormatSpec] = &[FormatSpec::new("application/x-other", "other")];
            match self.format {
                "test" => TEST,
                "other" => OTHER,
                _ => unreachable!("test handler format"),
            }
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

    fn handler(id: &'static str, format: &'static str, detects: bool) -> TestHandler {
        TestHandler {
            id,
            format,
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
    fn explicit_format_falls_back_when_content_is_not_detected() {
        let h = handler("explicit", "test", false);
        let detections = Arc::clone(&h.detections);
        let registry = HandlerRegistry::new().with_handler(h);
        for format in [" .TeSt ", "APPLICATION/X-TEST"] {
            let mut src = Cursor::new(b"TESTpayload");
            src.set_position(5);
            let structure = Structure::parse(&registry, &mut src, Some(format)).unwrap();
            assert_eq!(structure.container_id(), "explicit");
            assert_eq!(structure.total_size(), 11);
            let (structure, _) = drive(
                Structure::parse_task(&registry, 11, Some(format)),
                b"TESTpayload",
            );
            assert_eq!(structure.unwrap().container_id(), "explicit");
        }
        assert!(detections.load(Ordering::Relaxed) > 0);
        assert!(matches!(
            Structure::parse(&registry, &mut Cursor::new(b"TEST"), None),
            Err(Error::UnsupportedFormat)
        ));
    }

    #[test]
    fn detection_and_format_selection_prefer_the_latest_registration() {
        let base = HandlerRegistry::new().with_handler(handler("base", "test", true));
        let mut registry = HandlerRegistry::new();
        registry.extend_from(&base);
        registry.add_handler(handler("first", "test", true));
        registry.add_handler(handler("latest", "test", true));
        for format in [None, Some("test")] {
            let structure = Structure::parse(&registry, &mut Cursor::new(b"TEST"), format).unwrap();
            assert_eq!(structure.container_id(), "latest");
            let (structure, _) = drive(Structure::parse_task(&registry, 4, format), b"TEST");
            assert_eq!(structure.unwrap().container_id(), "latest");
        }
        // The registry composed from is unchanged.
        for format in [None, Some("test")] {
            assert_eq!(
                Structure::parse(&base, &mut Cursor::new(b"TEST"), format)
                    .unwrap()
                    .container_id(),
                "base"
            );
        }
    }

    #[test]
    fn content_detection_precedes_format_fallback() {
        let h = handler("test", "test", true);
        let detections = Arc::clone(&h.detections);
        let registry = HandlerRegistry::new().with_handler(h);
        for format in [Some("unknown"), Some("")] {
            assert_eq!(
                Structure::parse(&registry, &mut Cursor::new(b"TEST"), format)
                    .unwrap()
                    .container_id(),
                "test"
            );
            let (result, requests) = drive(Structure::parse_task(&registry, 4, format), b"TEST");
            assert_eq!(result.unwrap().container_id(), "test");
            assert!(requests > 0);
        }
        let mismatch_registry = HandlerRegistry::new()
            .with_handler(handler("fallback", "other", false))
            .with_handler(handler("detected", "test", true));
        assert_eq!(
            Structure::parse(&mismatch_registry, &mut Cursor::new(b"TEST"), Some("other"))
                .unwrap()
                .container_id(),
            "detected"
        );
        assert!(matches!(
            Structure::parse(&registry, &mut Cursor::new(b"nope"), Some("unknown")),
            Err(Error::UnsupportedFormat)
        ));
        let (result, _) = drive(
            Structure::parse_task(&registry, 4, Some("unknown")),
            b"nope",
        );
        assert!(matches!(result, Err(Error::UnsupportedFormat)));

        assert!(matches!(
            Structure::parse(&registry, &mut Cursor::new(b"nope"), Some("test")),
            Err(Error::InvalidFormat(_))
        ));
        assert!(detections.load(Ordering::Relaxed) > 0);
        for bytes in [b"".as_slice(), b"T", b"nope"] {
            assert!(matches!(
                Structure::parse(&registry, &mut Cursor::new(bytes), None),
                Err(Error::UnsupportedFormat)
            ));
        }
        let (result, _) = drive(Structure::parse_task(&registry, 4, Some("test")), b"T");
        assert!(matches!(
            result,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof
        ));
    }

    #[test]
    fn async_parsing_composes_with_later_reads_in_the_same_task() {
        let registry = HandlerRegistry::new().with_handler(handler("test", "test", true));
        for format in [None, Some("test")] {
            let registry = &registry;
            let task = Task::new(11, |mut io| async move {
                let structure = Structure::parse_async(registry, &mut io, format).await?;
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
        let registry = HandlerRegistry::new().with_handler(handler("test", "test", true));
        for format in [None, Some("test")] {
            assert!(matches!(
                Structure::parse(&registry, &mut Broken, format),
                Err(Error::Io(error)) if error.to_string() == "read failed"
            ));
        }
    }
}
