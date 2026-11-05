// Copyright 2026 Adobe. All rights reserved.
// This file is licensed to you under the Apache License,
// Version 2.0 (http://www.apache.org/licenses/LICENSE-2.0)
// or the MIT license (http://opensource.org/licenses/MIT),
// at your option.

//! Inspect a remote asset using HTTP byte ranges instead of downloading it.
//!
//! Run:
//! `cargo run --example https_range --features all-formats,tokio,https-example -- https://example.com/video.mp4`

use std::{
    future::Future,
    io,
    pin::Pin,
    task::{Context, Poll},
};

use asset_io::{tokio as asset, HandlerRegistry, Result};
use reqwest::{
    header::{ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE, RANGE},
    Client, StatusCode,
};
use tokio::io::{AsyncRead, AsyncSeek, ReadBuf};

const RANGE_CHUNK_SIZE: u64 = 64 * 1024;
type RangeFuture = Pin<Box<dyn Future<Output = io::Result<Vec<u8>>> + Send>>;

struct RemoteFile {
    client: Client,
    url: String,
    length: u64,
    position: u64,
    buffer: Vec<u8>,
    buffer_position: usize,
    pending: Option<RangeFuture>,
    range_count: u64,
    range_bytes: u64,
}

impl RemoteFile {
    async fn open(client: Client, url: String) -> io::Result<Self> {
        let response = client
            .head(&url)
            .header(ACCEPT_ENCODING, "identity")
            .send()
            .await
            .map_err(io::Error::other)?;
        if !response.status().is_success() {
            return Err(io::Error::other(format!(
                "HTTP HEAD returned {}",
                response.status()
            )));
        }
        require_identity_encoding(response.headers())?;
        let length = response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| io::Error::other("HTTP HEAD response has no valid content length"))?;

        Ok(Self {
            client,
            url,
            length,
            position: 0,
            buffer: Vec::new(),
            buffer_position: 0,
            pending: None,
            range_count: 0,
            range_bytes: 0,
        })
    }

    fn fetch(&self, start: u64, requested_end: u64) -> RangeFuture {
        let client = self.client.clone();
        let url = self.url.clone();
        let length = self.length;
        Box::pin(async move {
            let response = client
                .get(url)
                .header(ACCEPT_ENCODING, "identity")
                .header(RANGE, format!("bytes={start}-{requested_end}"))
                .send()
                .await
                .map_err(io::Error::other)?;

            if response.status() != StatusCode::PARTIAL_CONTENT {
                return Err(io::Error::other(format!(
                    "HTTP Range request returned {}, expected 206 Partial Content",
                    response.status()
                )));
            }
            require_identity_encoding(response.headers())?;

            let content_range = response
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| io::Error::other("HTTP response has no valid Content-Range"))?;
            let (actual_start, actual_end, actual_length) = parse_content_range(content_range)?;
            if actual_start != start
                || actual_end > requested_end
                || actual_end < actual_start
                || actual_length != length
            {
                return Err(io::Error::other(format!(
                    "unexpected Content-Range: {content_range}"
                )));
            }

            let bytes = response.bytes().await.map_err(io::Error::other)?;
            if bytes.len() as u64 != actual_end - actual_start + 1 {
                return Err(io::Error::other(
                    "HTTP range body length does not match Content-Range",
                ));
            }
            Ok(bytes.to_vec())
        })
    }
}

fn require_identity_encoding(headers: &reqwest::header::HeaderMap) -> io::Result<()> {
    if headers
        .get(CONTENT_ENCODING)
        .is_some_and(|value| !value.as_bytes().eq_ignore_ascii_case(b"identity"))
    {
        return Err(io::Error::other(
            "HTTP response used a non-identity Content-Encoding",
        ));
    }
    Ok(())
}

fn parse_content_range(value: &str) -> io::Result<(u64, u64, u64)> {
    let value = value
        .strip_prefix("bytes ")
        .ok_or_else(|| io::Error::other("unsupported Content-Range unit"))?;
    let (range, length) = value
        .split_once('/')
        .ok_or_else(|| io::Error::other("invalid Content-Range"))?;
    let (start, end) = range
        .split_once('-')
        .ok_or_else(|| io::Error::other("invalid Content-Range"))?;
    let parse = |part: &str| {
        part.parse::<u64>()
            .map_err(|_| io::Error::other("invalid Content-Range number"))
    };
    Ok((parse(start)?, parse(end)?, parse(length)?))
}

impl AsyncRead for RemoteFile {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            if self.buffer_position < self.buffer.len() {
                let count = buf
                    .remaining()
                    .min(self.buffer.len() - self.buffer_position);
                buf.put_slice(&self.buffer[self.buffer_position..self.buffer_position + count]);
                self.buffer_position += count;
                self.position += count as u64;
                return Poll::Ready(Ok(()));
            }

            if self.position >= self.length || buf.remaining() == 0 {
                return Poll::Ready(Ok(()));
            }

            if self.pending.is_none() {
                let start = self.position;
                let end = start
                    .saturating_add(RANGE_CHUNK_SIZE - 1)
                    .min(self.length - 1);
                self.pending = Some(self.fetch(start, end));
            }

            let result = self
                .pending
                .as_mut()
                .expect("range future was just initialized")
                .as_mut()
                .poll(cx);
            match result {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(result) => {
                    self.pending = None;
                    match result {
                        Ok(bytes) if !bytes.is_empty() => {
                            self.range_count += 1;
                            self.range_bytes += bytes.len() as u64;
                            self.buffer = bytes;
                            self.buffer_position = 0;
                        }
                        Ok(_) => {
                            return Poll::Ready(Err(io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "HTTP range response was empty before end of asset",
                            )));
                        }
                        Err(error) => return Poll::Ready(Err(error)),
                    }
                }
            }
        }
    }
}

impl AsyncSeek for RemoteFile {
    fn start_seek(mut self: Pin<&mut Self>, position: io::SeekFrom) -> io::Result<()> {
        let next = match position {
            io::SeekFrom::Start(position) => i128::from(position),
            io::SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
            io::SeekFrom::End(offset) => i128::from(self.length) + i128::from(offset),
        };
        if !(0..=i128::from(u64::MAX)).contains(&next) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek position is outside the supported range",
            ));
        }
        self.position = next as u64;
        self.buffer.clear();
        self.buffer_position = 0;
        self.pending = None;
        Ok(())
    }

    fn poll_complete(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<u64>> {
        Poll::Ready(Ok(self.position))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let Some(url) = std::env::args().nth(1) else {
        eprintln!("Usage: https_range <https-url>");
        std::process::exit(2);
    };
    let parsed_url = reqwest::Url::parse(&url).map_err(io::Error::other)?;
    if parsed_url.scheme() != "https" {
        return Err(io::Error::other("URL must use HTTPS").into());
    }

    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.url().scheme() == "https" && attempt.previous().len() < 10 {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .map_err(io::Error::other)?;
    let mut source = RemoteFile::open(client, url.clone()).await?;
    let structure = asset::parse(&HandlerRegistry::builtin(), &mut source, None).await?;

    println!(
        "{url}\n  {} ({}), {} bytes, {} segments",
        structure.media_type(),
        structure.container_id(),
        structure.total_size(),
        structure.segments().len()
    );
    println!(
        "  {} HTTP range request(s), {} asset bytes transferred",
        source.range_count, source.range_bytes
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, sync::Arc};

    use tokio::{
        io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
        net::TcpListener,
        sync::Mutex,
    };

    use super::*;

    async fn start_server(
        body: Option<Vec<u8>>,
        length: u64,
    ) -> (
        String,
        Arc<Mutex<Vec<(u64, u64)>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address: SocketAddr = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let observed = requests.clone();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let mut request = Vec::new();
                let mut byte = [0; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    if stream.read_exact(&mut byte).await.is_err() {
                        break;
                    }
                    request.push(byte[0]);
                }
                let request = String::from_utf8_lossy(&request);
                if request.starts_with("HEAD ") {
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n"
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    continue;
                }

                let Some(range) = request.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    (name.eq_ignore_ascii_case("range"))
                        .then(|| value.trim().strip_prefix("bytes="))
                        .flatten()
                }) else {
                    let _ = stream
                        .write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n")
                        .await;
                    continue;
                };
                let Some((start, end)) = range.split_once('-') else {
                    continue;
                };
                let (Ok(start), Ok(end)) = (start.parse::<u64>(), end.parse::<u64>()) else {
                    continue;
                };
                observed.lock().await.push((start, end));
                let actual_end = end.min(length.saturating_sub(1));
                let data = match &body {
                    Some(body) => body[start as usize..=actual_end as usize].to_vec(),
                    None => vec![0x5a; (actual_end - start + 1) as usize],
                };
                let response = format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{actual_end}/{length}\r\nConnection: close\r\n\r\n",
                    data.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.write_all(&data).await;
            }
        });
        (format!("http://{address}/asset"), requests, server)
    }

    #[tokio::test]
    async fn parses_remote_asset_through_ranges() {
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\0IEND\xae\x42\x60\x82".to_vec();
        let (url, requests, server) = start_server(Some(png.clone()), png.len() as u64).await;
        let mut source = RemoteFile::open(Client::new(), url).await.unwrap();
        let structure = asset::parse(&HandlerRegistry::builtin(), &mut source, None)
            .await
            .unwrap();
        assert_eq!(structure.total_size(), png.len() as u64);
        assert!(!requests.lock().await.is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn seeks_and_reads_ranges_above_four_gibibytes() {
        let length = 6 * 1024 * 1024 * 1024u64;
        let (url, requests, server) = start_server(None, length).await;
        let mut source = RemoteFile::open(Client::new(), url).await.unwrap();
        let offset = 5 * 1024 * 1024 * 1024u64 + 123;
        source.seek(io::SeekFrom::Start(offset)).await.unwrap();
        let mut byte = [0];
        source.read_exact(&mut byte).await.unwrap();
        assert_eq!(byte, [0x5a]);
        let ranges = requests.lock().await;
        assert_eq!(ranges[0].0, offset);
        assert!(ranges[0].1 >= offset);
        assert!(ranges[0].1 - ranges[0].0 + 1 <= RANGE_CHUNK_SIZE);
        assert!(source.range_bytes <= RANGE_CHUNK_SIZE);
        drop(ranges);
        server.abort();
    }
}
