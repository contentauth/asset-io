// Copyright 2026 Adobe. All rights reserved.
// This file is licensed to you under the Apache License,
// Version 2.0 (http://www.apache.org/licenses/LICENSE-2.0)
// or the MIT license (http://opensource.org/licenses/MIT),
// at your option.

//! Async stream adapters, enabled by the `tokio` feature.
//!
//! These functions drive the sans-I/O core with Tokio's [`AsyncRead`],
//! [`AsyncSeek`] and [`AsyncWrite`] traits. No blocking I/O or executor is used
//! inside the adapters. Sources must be seekable and remain unchanged between
//! parsing and subsequent reads or writes. Sink callbacks run synchronously on
//! the calling task and should not block.
//!
//! ```no_run
//! # async fn example() -> asset_io::Result<()> {
//! let registry = asset_io::HandlerRegistry::builtin();
//! let mut src = tokio::fs::File::open("image.jpg").await?;
//! let structure = asset_io::tokio::parse(&registry, &mut src, None).await?;
//! let updates = asset_io::Updates::new().remove_jumbf();
//! let mut out = tokio::fs::File::create("out.jpg").await?;
//! asset_io::tokio::write(
//!     &structure,
//!     &mut src,
//!     &mut out,
//!     &updates,
//!     &mut asset_io::NullSink,
//! )
//! .await?;
//! # Ok(())
//! # }
//! ```

use std::io::SeekFrom;

use ::tokio::io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt, AsyncWrite, AsyncWriteExt};

use crate::{
    plan::WritePlan, Action, HandlerRegistry, Item, Progress, Result, Sink, Structure, Task,
    Updates,
};

/// Run a task using asynchronous reads from `src`.
///
/// This also supports custom computations composed from the core's `*_async`
/// methods. Adjacent requests avoid seeking; each task starts with an unknown
/// source position. Errors and cancellation may leave the source repositioned.
pub async fn run<T, R>(mut task: Task<'_, T>, src: &mut R) -> Result<T>
where
    R: AsyncRead + AsyncSeek + Unpin + ?Sized,
{
    let mut at = None;
    loop {
        match task.resume()? {
            Progress::Read(mut req) => {
                if at != Some(req.offset()) {
                    src.seek(SeekFrom::Start(req.offset())).await?;
                }
                let offset = req.offset();
                let window = req.window_len() as u64;
                let buf = req.buffer();
                buf.clear();
                (&mut *src).take(window).read_to_end(buf).await?;
                at = Some(offset + buf.len() as u64);
            }
            Progress::Done(value) => return Ok(value),
        }
    }
}

/// Parse `src` using an explicit extension or MIME type, or detect it with `None`.
///
/// An unsupported explicit format fails without falling back to detection.
/// Stream length is determined by seeking to its end; parsing uses absolute
/// offsets from the beginning, not its current position.
pub async fn parse<R>(
    registry: &HandlerRegistry,
    src: &mut R,
    format: Option<&str>,
) -> Result<Structure>
where
    R: AsyncRead + AsyncSeek + Unpin + ?Sized,
{
    let total = src.seek(SeekFrom::End(0)).await?;
    run(Structure::parse_task(registry, total, format), src).await
}

/// Read an item's stored bytes, concatenating its extents without decoding.
pub async fn read_item<R>(structure: &Structure, src: &mut R, item: &Item) -> Result<Vec<u8>>
where
    R: AsyncRead + AsyncSeek + Unpin + ?Sized,
{
    run(
        Task::new(structure.total_size(), |mut io| async move {
            structure.read_item_async(&mut io, item).await
        }),
        src,
    )
    .await
}

/// Read an item's content, undoing its transfer encoding when supported.
pub async fn read_content<R>(structure: &Structure, src: &mut R, item: &Item) -> Result<Vec<u8>>
where
    R: AsyncRead + AsyncSeek + Unpin + ?Sized,
{
    run(
        Task::new(structure.total_size(), |mut io| async move {
            structure.read_content_async(&mut io, item).await
        }),
        src,
    )
    .await
}

/// Find the C2PA manifest-store item, reading only if its type was not recorded.
pub async fn c2pa_item<'s, R>(structure: &'s Structure, src: &mut R) -> Result<Option<&'s Item>>
where
    R: AsyncRead + AsyncSeek + Unpin + ?Sized,
{
    run(
        Task::new(structure.total_size(), |mut io| async move {
            structure.c2pa_item_async(&mut io).await
        }),
        src,
    )
    .await
}

/// Read the decoded C2PA store, combining BMFF original/update stores.
pub async fn c2pa_store<R>(structure: &Structure, src: &mut R) -> Result<Option<Vec<u8>>>
where
    R: AsyncRead + AsyncSeek + Unpin + ?Sized,
{
    run(
        Task::new(structure.total_size(), |mut io| async move {
            structure.c2pa_store_async(&mut io).await
        }),
        src,
    )
    .await
}

/// Plan a write against the source from which `structure` was parsed.
pub async fn write_plan<'a, R>(
    structure: &Structure,
    src: &mut R,
    updates: &'a Updates,
) -> Result<WritePlan<'a>>
where
    R: AsyncRead + AsyncSeek + Unpin + ?Sized,
{
    run(structure.write_plan_task(updates), src).await
}

/// Execute a plan, writing from `out`'s current position and flushing on success.
///
/// Sink offsets are relative to the first byte written. Errors, sink
/// cancellation, or dropping this future can leave partial output; the operation
/// is not transactional. An existing destination is not truncated.
pub async fn execute<R, W, S>(
    plan: &WritePlan<'_>,
    src: &mut R,
    out: &mut W,
    sink: &mut S,
) -> Result<()>
where
    R: AsyncRead + AsyncSeek + Unpin + ?Sized,
    W: AsyncWrite + Unpin + ?Sized,
    S: Sink + ?Sized,
{
    let mut execution = plan.start();
    let mut at = None;
    loop {
        match execution.step(sink)? {
            Action::Read { offset, buf } => {
                if at != Some(offset) {
                    src.seek(SeekFrom::Start(offset)).await?;
                }
                src.read_exact(buf).await?;
                at = Some(offset + buf.len() as u64);
            }
            Action::Write(bytes) => out.write_all(bytes).await?,
            Action::Done => break,
        }
    }
    out.flush().await?;
    Ok(())
}

/// Plan and execute a write, returning the output layout without reparsing.
///
/// Output positioning, flushing, and partial-output behavior match [`execute`].
pub async fn write<R, W, S>(
    structure: &Structure,
    src: &mut R,
    out: &mut W,
    updates: &Updates,
    sink: &mut S,
) -> Result<Structure>
where
    R: AsyncRead + AsyncSeek + Unpin + ?Sized,
    W: AsyncWrite + Unpin + ?Sized,
    S: Sink + ?Sized,
{
    let plan = write_plan(structure, src, updates).await?;
    execute(&plan, src, out, sink).await?;
    Ok(plan.into_structure())
}
