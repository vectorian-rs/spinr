//! Shared-nothing HTTP/1.1 benchmark engine.

use crate::loadtest::types::{
    EngineConfig, EngineMode, HdrLatencyHistogram, HttpMethod, RawWorkerMetrics,
};
use mio::net::TcpStream;
use mio::{Events, Interest, Poll, Token};
use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("failed to create mio::Poll: {0}")]
    PollCreate(io::Error),
    #[error("failed to register TCP stream: {0}")]
    Register(io::Error),
    #[error("{0}")]
    Io(#[from] io::Error),
}

const STATUS_CODE_SLOTS: usize = 600;
const DEFAULT_EVENT_CAPACITY: usize = 1024;

#[derive(Debug, Clone, Copy)]
struct RunWindow {
    warmup_deadline: Instant,
    measurement_deadline: Instant,
    can_start_requests: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ResponseAccounting {
    payload_bytes: u64,
    wire_bytes: u64,
}

impl ResponseAccounting {
    fn payload_only(bytes: u64) -> Self {
        Self {
            payload_bytes: bytes,
            wire_bytes: bytes,
        }
    }
}

impl std::ops::AddAssign for ResponseAccounting {
    fn add_assign(&mut self, rhs: Self) {
        self.payload_bytes += rhs.payload_bytes;
        self.wire_bytes += rhs.wire_bytes;
    }
}

impl RawWorkerMetrics {
    fn new(worker_id: u32, corrected: bool) -> Self {
        Self {
            worker_id,
            request_count: 0,
            success_count: 0,
            error_count: 0,
            status_counts: [0; STATUS_CODE_SLOTS],
            latency_uncorrected: HdrLatencyHistogram::default(),
            latency_corrected: corrected.then(HdrLatencyHistogram::default),
            duration_secs: 0.0,
            payload_bytes: 0,
            wire_bytes: 0,
        }
    }

    fn record_response(
        &mut self,
        status_code: Option<u16>,
        request_start: Instant,
        scheduled_start: Option<Instant>,
        accounting: ResponseAccounting,
        is_transport_error: bool,
    ) {
        self.request_count += 1;
        self.payload_bytes += accounting.payload_bytes;
        self.wire_bytes += accounting.wire_bytes;

        let uncorrected_us = request_start.elapsed().as_micros() as u64;
        self.latency_uncorrected.record(uncorrected_us.max(1));

        if let Some(scheduled_start) = scheduled_start
            && let Some(corrected) = self.latency_corrected.as_mut()
        {
            corrected.record(scheduled_start.elapsed().as_micros() as u64);
        }

        if let Some(status_code) = status_code
            && let Some(slot) = self.status_counts.get_mut(status_code as usize)
        {
            *slot += 1;
        }

        if is_transport_error {
            self.error_count += 1;
            return;
        }

        match status_code {
            Some(200..=299) => self.success_count += 1,
            Some(_) | None => self.error_count += 1,
        }
    }
}

pub fn run(config: EngineConfig, request_bytes: &[u8]) -> Result<RawWorkerMetrics, EngineError> {
    run_for_durations(
        &config,
        request_bytes,
        Duration::from_secs(config.warmup_seconds as u64),
        Duration::from_secs(config.duration_seconds as u64),
    )
}

fn run_for_durations(
    config: &EngineConfig,
    request_bytes: &[u8],
    warmup_duration: Duration,
    measurement_duration: Duration,
) -> Result<RawWorkerMetrics, EngineError> {
    if config.connections == 0 {
        return Ok(RawWorkerMetrics::new(
            config.worker_id,
            matches!(
                config.mode,
                EngineMode::RateLimited {
                    latency_correction: true,
                    ..
                }
            ),
        ));
    }

    let mut poll = Poll::new().map_err(EngineError::PollCreate)?;
    let mut events = Events::with_capacity(DEFAULT_EVENT_CAPACITY.max(config.connections as usize));

    let start = Instant::now();
    let warmup_deadline = start + warmup_duration;
    let measurement_deadline = warmup_deadline + measurement_duration;
    let mut measurement_started = warmup_duration.is_zero();
    let mut metrics = RawWorkerMetrics::new(
        config.worker_id,
        matches!(
            config.mode,
            EngineMode::RateLimited {
                latency_correction: true,
                ..
            }
        ),
    );
    let rates = match config.mode {
        EngineMode::MaxThroughput => vec![0; config.connections as usize],
        EngineMode::RateLimited {
            requests_per_second,
            ..
        } => distribute_rate(requests_per_second, config.connections as usize),
    };

    let mut connections = Vec::with_capacity(config.connections as usize);
    for (index, &rate_share) in rates.iter().enumerate() {
        connections.push(Connection::connect(
            &mut poll,
            Token(index),
            config.remote_addr,
            config.read_buffer_size.max(1024),
            rate_share,
            start,
        )?);
    }

    loop {
        let now = Instant::now();
        if !measurement_started && now >= warmup_deadline {
            measurement_started = true;
            if matches!(config.mode, EngineMode::RateLimited { .. }) {
                for connection in &mut connections {
                    connection.reset_rate_phase(warmup_deadline);
                    if matches!(connection.state, ConnectionState::Idle) {
                        connection
                            .reregister(&poll, connection.interest(&config.mode, now, false))?;
                    }
                }
            }
        }

        let window = RunWindow {
            warmup_deadline,
            measurement_deadline,
            can_start_requests: now < measurement_deadline,
        };
        for connection in &mut connections {
            connection.maybe_start_request(&poll, &config.mode, request_bytes, now, window)?;
        }

        if !window.can_start_requests && connections.iter().all(Connection::is_quiescent) {
            break;
        }

        let timeout = poll_timeout(
            &connections,
            &config.mode,
            now,
            warmup_deadline,
            measurement_deadline,
        );
        poll.poll(&mut events, timeout)?;

        for event in &events {
            let connection = &mut connections[event.token().0];

            if event.is_error() || event.is_read_closed() || event.is_write_closed() {
                handle_connection_failure(
                    &mut poll,
                    connection,
                    &mut metrics,
                    window.can_start_requests,
                )?;
                continue;
            }

            if event.is_writable() {
                handle_writable(
                    &mut poll,
                    connection,
                    &mut metrics,
                    request_bytes,
                    &config.mode,
                    window,
                    window.can_start_requests,
                )?;
            }

            if event.is_readable() {
                handle_readable(
                    &mut poll,
                    connection,
                    &mut metrics,
                    config.method != HttpMethod::HEAD,
                    window.can_start_requests,
                )?;
            }
        }
    }

    metrics.duration_secs = measurement_deadline
        .saturating_duration_since(warmup_deadline)
        .as_secs_f64();

    Ok(metrics)
}

fn distribute_rate(total_rps: u64, slots: usize) -> Vec<u64> {
    if slots == 0 {
        return Vec::new();
    }

    let base = total_rps / slots as u64;
    let remainder = total_rps % slots as u64;
    (0..slots)
        .map(|index| {
            if index < remainder as usize {
                base + 1
            } else {
                base
            }
        })
        .collect()
}

fn poll_timeout(
    connections: &[Connection],
    mode: &EngineMode,
    now: Instant,
    warmup_deadline: Instant,
    measurement_deadline: Instant,
) -> Option<Duration> {
    let mut timeout = if now < warmup_deadline {
        Some(warmup_deadline.saturating_duration_since(now))
    } else if now < measurement_deadline {
        Some(measurement_deadline.saturating_duration_since(now))
    } else {
        None
    };

    if let EngineMode::RateLimited { .. } = mode {
        for connection in connections {
            if let Some(next_due) = connection.next_due_at() {
                let due = next_due.saturating_duration_since(now);
                timeout = match timeout {
                    Some(current) => Some(current.min(due)),
                    None => Some(due),
                };
            }
        }
    }

    timeout
}

fn handle_connection_failure(
    poll: &mut Poll,
    connection: &mut Connection,
    metrics: &mut RawWorkerMetrics,
    allow_reconnect: bool,
) -> Result<(), EngineError> {
    if let Some((request_start, scheduled_start, measure_request)) = connection.fail_request()
        && measure_request
    {
        metrics.record_response(
            None,
            request_start,
            scheduled_start,
            ResponseAccounting::default(),
            true,
        );
    }

    connection.close(poll);
    if allow_reconnect {
        connection.reconnect(poll)?;
    }

    Ok(())
}

fn handle_writable(
    poll: &mut Poll,
    connection: &mut Connection,
    metrics: &mut RawWorkerMetrics,
    request_bytes: &[u8],
    mode: &EngineMode,
    window: RunWindow,
    allow_reconnect: bool,
) -> Result<(), EngineError> {
    let now = Instant::now();

    match connection.state {
        ConnectionState::Connecting => {
            if let Some(stream) = connection.stream.as_mut()
                && let Some(_err) = stream.take_error()?
            {
                return handle_connection_failure(poll, connection, metrics, allow_reconnect);
            }

            connection.state = ConnectionState::Idle;
            connection.reregister(poll, connection.interest(mode, now, allow_reconnect))?;
            connection.maybe_start_request(poll, mode, request_bytes, now, window)?;
        }
        ConnectionState::Idle => {
            connection.maybe_start_request(poll, mode, request_bytes, now, window)?;
        }
        ConnectionState::Writing => {
            let Some(stream) = connection.stream.as_mut() else {
                return Ok(());
            };

            while connection.write_pos < request_bytes.len() {
                match stream.write(&request_bytes[connection.write_pos..]) {
                    Ok(0) => {
                        return handle_connection_failure(
                            poll,
                            connection,
                            metrics,
                            allow_reconnect,
                        );
                    }
                    Ok(written) => connection.write_pos += written,
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                    Err(_) => {
                        return handle_connection_failure(
                            poll,
                            connection,
                            metrics,
                            allow_reconnect,
                        );
                    }
                }
            }

            connection.state = ConnectionState::ReadingHead;
            connection.read_len = 0;
            connection.reregister(poll, Interest::READABLE)?;
        }
        _ => {}
    }

    Ok(())
}

fn handle_readable(
    poll: &mut Poll,
    connection: &mut Connection,
    metrics: &mut RawWorkerMetrics,
    expects_response_body: bool,
    allow_reconnect: bool,
) -> Result<(), EngineError> {
    match connection.state {
        ConnectionState::Idle => {
            let mut byte = [0u8; 1];
            let Some(stream) = connection.stream.as_mut() else {
                return Ok(());
            };
            match stream.read(&mut byte) {
                Ok(0) | Ok(_) | Err(_) => {
                    handle_connection_failure(poll, connection, metrics, allow_reconnect)?
                }
            }
        }
        ConnectionState::ReadingHead => {
            let Some(stream) = connection.stream.as_mut() else {
                return Ok(());
            };

            loop {
                if connection.read_len == connection.read_buf.len() {
                    return handle_connection_failure(poll, connection, metrics, allow_reconnect);
                }

                match stream.read(&mut connection.read_buf[connection.read_len..]) {
                    Ok(0) => {
                        return handle_connection_failure(
                            poll,
                            connection,
                            metrics,
                            allow_reconnect,
                        );
                    }
                    Ok(read) => {
                        connection.read_len += read;
                        match parse_response_head(
                            &connection.read_buf[..connection.read_len],
                            expects_response_body,
                        )? {
                            Some(parsed) => {
                                let body_bytes_in_buffer =
                                    connection.read_len.saturating_sub(parsed.header_len) as u64;
                                let status_code = parsed.status_code;
                                let measure_request = connection.measure_request;
                                let request_start =
                                    connection.request_start.unwrap_or_else(Instant::now);
                                let scheduled_start = connection.scheduled_start;
                                let should_close = parsed.connection_close;

                                match parsed.body_kind {
                                    BodyKind::None => {
                                        if measure_request {
                                            metrics.record_response(
                                                Some(status_code),
                                                request_start,
                                                scheduled_start,
                                                ResponseAccounting::default(),
                                                false,
                                            );
                                        }
                                        connection.finish_response(
                                            poll,
                                            should_close,
                                            allow_reconnect,
                                        )?;
                                        return Ok(());
                                    }
                                    BodyKind::Fixed(total_len) => {
                                        if body_bytes_in_buffer >= total_len as u64 {
                                            if measure_request {
                                                metrics.record_response(
                                                    Some(status_code),
                                                    request_start,
                                                    scheduled_start,
                                                    ResponseAccounting::payload_only(
                                                        total_len as u64,
                                                    ),
                                                    false,
                                                );
                                            }
                                            connection.finish_response(
                                                poll,
                                                should_close,
                                                allow_reconnect,
                                            )?;
                                            return Ok(());
                                        }

                                        connection.state = ConnectionState::DrainingContentLength {
                                            remaining: total_len as u64 - body_bytes_in_buffer,
                                            status_code,
                                            total_body_len: total_len as u64,
                                            should_close,
                                        };
                                        connection.read_len = 0;
                                        connection.reregister(poll, Interest::READABLE)?;
                                        return Ok(());
                                    }
                                    BodyKind::Chunked => {
                                        let body_start = parsed.header_len;
                                        let buffered =
                                            &connection.read_buf[body_start..connection.read_len];
                                        let mut decoder = ChunkedDecoder::new();
                                        let progress = decoder.feed(buffered);
                                        if progress.complete {
                                            if measure_request {
                                                metrics.record_response(
                                                    Some(status_code),
                                                    request_start,
                                                    scheduled_start,
                                                    progress.accounting,
                                                    false,
                                                );
                                            }
                                            connection.finish_response(
                                                poll,
                                                should_close,
                                                allow_reconnect,
                                            )?;
                                            return Ok(());
                                        }
                                        connection.state = ConnectionState::DrainingChunked {
                                            status_code,
                                            should_close,
                                            decoder,
                                            accounting: progress.accounting,
                                        };
                                        connection.read_len = 0;
                                        connection.reregister(poll, Interest::READABLE)?;
                                        return Ok(());
                                    }
                                }
                            }
                            None => continue,
                        }
                    }
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                    Err(_) => {
                        return handle_connection_failure(
                            poll,
                            connection,
                            metrics,
                            allow_reconnect,
                        );
                    }
                }
            }
        }
        ConnectionState::DrainingContentLength {
            remaining,
            status_code,
            total_body_len,
            should_close,
        } => {
            let Some(stream) = connection.stream.as_mut() else {
                return Ok(());
            };

            let mut remaining = remaining;
            loop {
                match stream.read(&mut connection.read_buf[..]) {
                    Ok(0) => {
                        return handle_connection_failure(
                            poll,
                            connection,
                            metrics,
                            allow_reconnect,
                        );
                    }
                    Ok(read) => {
                        remaining = remaining.saturating_sub(read as u64);
                        if remaining == 0 {
                            if connection.measure_request {
                                metrics.record_response(
                                    Some(status_code),
                                    connection.request_start.unwrap_or_else(Instant::now),
                                    connection.scheduled_start,
                                    ResponseAccounting::payload_only(total_body_len),
                                    false,
                                );
                            }
                            connection.finish_response(poll, should_close, allow_reconnect)?;
                            return Ok(());
                        }
                        connection.state = ConnectionState::DrainingContentLength {
                            remaining,
                            status_code,
                            total_body_len,
                            should_close,
                        };
                    }
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        connection.state = ConnectionState::DrainingContentLength {
                            remaining,
                            status_code,
                            total_body_len,
                            should_close,
                        };
                        return Ok(());
                    }
                    Err(_) => {
                        return handle_connection_failure(
                            poll,
                            connection,
                            metrics,
                            allow_reconnect,
                        );
                    }
                }
            }
        }
        ConnectionState::DrainingChunked {
            status_code,
            should_close,
            mut decoder,
            mut accounting,
        } => {
            let Some(stream) = connection.stream.as_mut() else {
                return Ok(());
            };

            loop {
                match stream.read(&mut connection.read_buf[..]) {
                    Ok(0) => {
                        return handle_connection_failure(
                            poll,
                            connection,
                            metrics,
                            allow_reconnect,
                        );
                    }
                    Ok(read) => {
                        let progress = decoder.feed(&connection.read_buf[..read]);
                        accounting += progress.accounting;
                        if progress.complete {
                            if connection.measure_request {
                                metrics.record_response(
                                    Some(status_code),
                                    connection.request_start.unwrap_or_else(Instant::now),
                                    connection.scheduled_start,
                                    accounting,
                                    false,
                                );
                            }
                            connection.finish_response(poll, should_close, allow_reconnect)?;
                            return Ok(());
                        }
                    }
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        connection.state = ConnectionState::DrainingChunked {
                            status_code,
                            should_close,
                            decoder,
                            accounting,
                        };
                        return Ok(());
                    }
                    Err(_) => {
                        return handle_connection_failure(
                            poll,
                            connection,
                            metrics,
                            allow_reconnect,
                        );
                    }
                }
            }
        }
        ConnectionState::Connecting | ConnectionState::Writing | ConnectionState::Closed => {}
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BodyKind {
    None,
    Fixed(usize),
    Chunked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedResponseHead {
    status_code: u16,
    header_len: usize,
    body_kind: BodyKind,
    connection_close: bool,
}

/// Classify the response body framing from method, status, and headers.
///
/// Extracted as a pure function so it can be tested and formally verified
/// without requiring mio/network state.
fn classify_body_kind(
    expects_body: bool,
    status_code: u16,
    has_chunked_te: bool,
    content_length: Option<usize>,
) -> BodyKind {
    if !expects_body || matches!(status_code, 100..=199 | 204 | 304) {
        BodyKind::None
    } else if has_chunked_te {
        BodyKind::Chunked
    } else {
        BodyKind::Fixed(content_length.unwrap_or(0))
    }
}

fn parse_response_head(buf: &[u8], expects_body: bool) -> io::Result<Option<ParsedResponseHead>> {
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut response = httparse::Response::new(&mut headers);
    match response.parse(buf) {
        Ok(httparse::Status::Partial) => Ok(None),
        Ok(httparse::Status::Complete(header_len)) => {
            let status_code = response.code.ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "response missing status code")
            })?;
            let connection_close = response.headers.iter().any(|header| {
                header.name.eq_ignore_ascii_case("connection")
                    && std::str::from_utf8(header.value)
                        .map(|value| value.eq_ignore_ascii_case("close"))
                        .unwrap_or(false)
            });

            let mut content_length = None;
            let mut chunked = false;
            for header in response.headers.iter() {
                if header.name.eq_ignore_ascii_case("content-length") {
                    let value = std::str::from_utf8(header.value).map_err(invalid_response)?;
                    content_length = Some(value.trim().parse::<usize>().map_err(invalid_response)?);
                }

                if header.name.eq_ignore_ascii_case("transfer-encoding") {
                    let value = std::str::from_utf8(header.value).map_err(invalid_response)?;
                    chunked = value
                        .split(',')
                        .any(|encoding| encoding.trim().eq_ignore_ascii_case("chunked"));
                }
            }

            let body_kind = classify_body_kind(expects_body, status_code, chunked, content_length);

            Ok(Some(ParsedResponseHead {
                status_code,
                header_len,
                body_kind,
                connection_close,
            }))
        }
        Err(err) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid HTTP response: {err}"),
        )),
    }
}

/// Incremental chunked transfer-encoding decoder.
///
/// Tracks state across partial reads so that chunk boundaries are parsed
/// correctly even when data arrives in arbitrary-sized pieces.
#[derive(Debug, Clone, Copy)]
enum ChunkedDecoder {
    /// Accumulating hex chunk-size line. `size` is the parsed value so far,
    /// `in_ext` is true after seeing `;' (chunk extension — ignored).
    SizeLine { size: u64, in_ext: bool },
    /// Skipping `remaining` bytes of chunk data.
    Data { remaining: u64 },
    /// Consuming the CRLF after chunk data (`need` counts bytes left: 2→1→0).
    PostDataCrlf { need: u8 },
    /// After the last-chunk (size 0): scanning for the empty line that ends
    /// the trailer section. `matched` counts consecutive bytes of `\r\n`.
    Trailers {
        line_has_bytes: bool,
        pending_cr: bool,
    },
    /// Terminal state — the complete chunked body has been consumed.
    Done,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ChunkedProgress {
    accounting: ResponseAccounting,
    complete: bool,
}

impl ChunkedDecoder {
    fn new() -> Self {
        ChunkedDecoder::SizeLine {
            size: 0,
            in_ext: false,
        }
    }

    fn is_done(&self) -> bool {
        matches!(self, ChunkedDecoder::Done)
    }

    /// Feed bytes into the decoder, returning wire and payload byte accounting
    /// for this chunk of input plus the completion state after the feed.
    fn feed(&mut self, buf: &[u8]) -> ChunkedProgress {
        let mut pos = 0;
        let mut payload_bytes = 0;
        while pos < buf.len() && !self.is_done() {
            match self {
                ChunkedDecoder::SizeLine { size, in_ext } => {
                    let b = buf[pos];
                    pos += 1;
                    if b == b'\n' {
                        // End of size line
                        if *size == 0 {
                            // Last chunk → scan trailers
                            *self = ChunkedDecoder::Trailers {
                                line_has_bytes: false,
                                pending_cr: false,
                            };
                        } else {
                            *self = ChunkedDecoder::Data { remaining: *size };
                        }
                    } else if b == b'\r' {
                        // Part of CRLF, ignore
                    } else if b == b';' {
                        *in_ext = true;
                    } else if !*in_ext && let Some(digit) = hex_digit(b) {
                        *size = size.wrapping_mul(16).wrapping_add(digit as u64);
                        // Non-hex chars before ';' are malformed; be lenient.
                    }
                }
                ChunkedDecoder::Data { remaining } => {
                    let avail = (buf.len() - pos) as u64;
                    let skip = avail.min(*remaining);
                    pos += skip as usize;
                    payload_bytes += skip;
                    *remaining -= skip;
                    if *remaining == 0 {
                        *self = ChunkedDecoder::PostDataCrlf { need: 2 };
                    }
                }
                ChunkedDecoder::PostDataCrlf { need } => {
                    pos += 1;
                    *need -= 1;
                    if *need == 0 {
                        *self = ChunkedDecoder::SizeLine {
                            size: 0,
                            in_ext: false,
                        };
                    }
                }
                ChunkedDecoder::Trailers {
                    line_has_bytes,
                    pending_cr,
                } => {
                    let b = buf[pos];
                    pos += 1;
                    if *pending_cr {
                        if b == b'\n' {
                            if *line_has_bytes {
                                *line_has_bytes = false;
                                *pending_cr = false;
                            } else {
                                *self = ChunkedDecoder::Done;
                            }
                        } else if b == b'\r' {
                            *line_has_bytes = true;
                            *pending_cr = true;
                        } else {
                            *line_has_bytes = true;
                            *pending_cr = false;
                        }
                    } else if b == b'\r' {
                        *pending_cr = true;
                    } else {
                        *line_has_bytes = true;
                    }
                }
                ChunkedDecoder::Done => break,
            }
        }
        ChunkedProgress {
            accounting: ResponseAccounting {
                payload_bytes,
                wire_bytes: pos as u64,
            },
            complete: self.is_done(),
        }
    }
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn invalid_response(err: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, err.to_string())
}

#[derive(Debug)]
struct Connection {
    token: Token,
    remote_addr: SocketAddr,
    stream: Option<TcpStream>,
    state: ConnectionState,
    read_buf: Box<[u8]>,
    read_len: usize,
    write_pos: usize,
    request_start: Option<Instant>,
    scheduled_start: Option<Instant>,
    measure_request: bool,
    rate_rps: u64,
    completed_in_phase: u64,
    phase_start: Instant,
}

#[derive(Debug, Clone, Copy)]
enum ConnectionState {
    Connecting,
    Idle,
    Writing,
    ReadingHead,
    DrainingContentLength {
        remaining: u64,
        status_code: u16,
        total_body_len: u64,
        should_close: bool,
    },
    DrainingChunked {
        status_code: u16,
        should_close: bool,
        decoder: ChunkedDecoder,
        accounting: ResponseAccounting,
    },
    Closed,
}

impl Connection {
    fn connect(
        poll: &mut Poll,
        token: Token,
        remote_addr: SocketAddr,
        read_buffer_size: usize,
        rate_rps: u64,
        phase_start: Instant,
    ) -> Result<Self, EngineError> {
        let mut stream = TcpStream::connect(remote_addr)?;
        stream.set_nodelay(true)?;
        poll.registry()
            .register(&mut stream, token, Interest::WRITABLE)
            .map_err(EngineError::Register)?;

        Ok(Self {
            token,
            remote_addr,
            stream: Some(stream),
            state: ConnectionState::Connecting,
            read_buf: vec![0u8; read_buffer_size].into_boxed_slice(),
            read_len: 0,
            write_pos: 0,
            request_start: None,
            scheduled_start: None,
            measure_request: false,
            rate_rps,
            completed_in_phase: 0,
            phase_start,
        })
    }

    fn close(&mut self, poll: &mut Poll) {
        if let Some(mut stream) = self.stream.take() {
            let _ = poll.registry().deregister(&mut stream);
        }
        self.state = ConnectionState::Closed;
        self.read_len = 0;
        self.write_pos = 0;
        self.request_start = None;
        self.scheduled_start = None;
        self.measure_request = false;
    }

    fn reconnect(&mut self, poll: &mut Poll) -> Result<(), EngineError> {
        self.close(poll);
        let mut stream = TcpStream::connect(self.remote_addr)?;
        stream.set_nodelay(true)?;
        poll.registry()
            .register(&mut stream, self.token, Interest::WRITABLE)?;
        self.stream = Some(stream);
        self.state = ConnectionState::Connecting;
        Ok(())
    }

    fn reregister(&mut self, poll: &Poll, interest: Interest) -> Result<(), EngineError> {
        if let Some(stream) = self.stream.as_mut() {
            poll.registry().reregister(stream, self.token, interest)?;
        }
        Ok(())
    }

    fn interest(&self, mode: &EngineMode, now: Instant, allow_reconnect: bool) -> Interest {
        match self.state {
            ConnectionState::Connecting | ConnectionState::Writing => Interest::WRITABLE,
            ConnectionState::ReadingHead
            | ConnectionState::DrainingContentLength { .. }
            | ConnectionState::DrainingChunked { .. } => Interest::READABLE,
            ConnectionState::Idle if !allow_reconnect => Interest::READABLE,
            ConnectionState::Idle => match mode {
                EngineMode::MaxThroughput => Interest::WRITABLE,
                EngineMode::RateLimited { .. } => {
                    if self.should_send(now, mode) {
                        Interest::WRITABLE
                    } else {
                        Interest::READABLE
                    }
                }
            },
            ConnectionState::Closed => Interest::READABLE,
        }
    }

    fn reset_rate_phase(&mut self, phase_start: Instant) {
        self.completed_in_phase = 0;
        self.phase_start = phase_start;
    }

    fn next_due_at(&self) -> Option<Instant> {
        if self.rate_rps == 0 || !matches!(self.state, ConnectionState::Idle) {
            return None;
        }
        Some(self.phase_start + schedule_offset(self.completed_in_phase, self.rate_rps))
    }

    fn should_send(&self, now: Instant, mode: &EngineMode) -> bool {
        match mode {
            EngineMode::MaxThroughput => true,
            EngineMode::RateLimited { .. } => match self.next_due_at() {
                Some(next_due) => now >= next_due,
                None => false,
            },
        }
    }

    fn maybe_start_request(
        &mut self,
        poll: &Poll,
        mode: &EngineMode,
        request_bytes: &[u8],
        now: Instant,
        window: RunWindow,
    ) -> Result<(), EngineError> {
        if !window.can_start_requests || !matches!(self.state, ConnectionState::Idle) {
            return Ok(());
        }
        if !self.should_send(now, mode) {
            return Ok(());
        }

        // Capture the due time before leaving Idle; next_due_at() is only
        // defined for idle connections.
        let scheduled_start = match mode {
            EngineMode::MaxThroughput => None,
            EngineMode::RateLimited {
                latency_correction, ..
            } if *latency_correction => self.next_due_at(),
            EngineMode::RateLimited { .. } => None,
        };

        self.state = ConnectionState::Writing;
        self.write_pos = 0;
        self.read_len = 0;
        self.request_start = Some(now);
        self.measure_request = now >= window.warmup_deadline && now < window.measurement_deadline;
        self.scheduled_start = scheduled_start;
        self.reregister(poll, Interest::WRITABLE)?;

        if request_bytes.is_empty() {
            self.state = ConnectionState::Idle;
        }

        Ok(())
    }

    fn finish_response(
        &mut self,
        poll: &mut Poll,
        should_close: bool,
        allow_reconnect: bool,
    ) -> Result<(), EngineError> {
        if self.rate_rps > 0 {
            self.completed_in_phase += 1;
        }

        self.request_start = None;
        self.scheduled_start = None;
        self.measure_request = false;
        self.read_len = 0;
        self.write_pos = 0;

        if should_close {
            if allow_reconnect {
                self.reconnect(poll)?;
            } else {
                self.close(poll);
            }
            return Ok(());
        }

        self.state = ConnectionState::Idle;
        Ok(())
    }

    fn fail_request(&mut self) -> Option<(Instant, Option<Instant>, bool)> {
        let request_start = self.request_start.take()?;
        let scheduled_start = self.scheduled_start.take();
        let measure_request = self.measure_request;
        self.measure_request = false;
        self.read_len = 0;
        self.write_pos = 0;

        if self.rate_rps > 0 {
            self.completed_in_phase += 1;
        }

        Some((request_start, scheduled_start, measure_request))
    }

    fn is_quiescent(&self) -> bool {
        matches!(self.state, ConnectionState::Idle | ConnectionState::Closed)
    }
}

fn schedule_offset(index: u64, rate_rps: u64) -> Duration {
    if rate_rps == 0 {
        Duration::ZERO
    } else {
        Duration::from_secs_f64(index as f64 / rate_rps as f64)
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    // ── Chunked decoder proofs ──

    #[kani::proof]
    #[kani::unwind(33)]
    fn proof_decoder_split_feed() {
        let len: usize = kani::any();
        kani::assume(len <= 32);
        let mut buf = [0u8; 32];
        for i in 0..len {
            buf[i] = kani::any();
        }
        let split: usize = kani::any();
        kani::assume(split <= len);

        // One-shot feed
        let mut dec_one = ChunkedDecoder::new();
        let p_one = dec_one.feed(&buf[..len]);

        // Two-part feed
        let mut dec_two = ChunkedDecoder::new();
        let p_first = dec_two.feed(&buf[..split]);
        let p_second = if !dec_two.is_done() {
            dec_two.feed(&buf[split..len])
        } else {
            ChunkedProgress::default()
        };

        assert!(
            dec_one.is_done() == dec_two.is_done(),
            "split-feed must reach same completion state"
        );
        assert!(
            p_one.accounting.payload_bytes
                == p_first.accounting.payload_bytes + p_second.accounting.payload_bytes,
            "split-feed must produce same payload bytes"
        );
    }

    #[kani::proof]
    #[kani::unwind(6)]
    fn proof_decoder_no_early_done() {
        let len: usize = kani::any();
        kani::assume(len > 0 && len < 5);
        let mut buf = [0u8; 4];
        for i in 0..len {
            buf[i] = kani::any();
        }

        let mut dec = ChunkedDecoder::new();
        dec.feed(&buf[..len]);

        // A valid chunked terminator is at minimum "0\r\n\r\n" = 5 bytes,
        // so < 5 bytes should never complete.
        assert!(
            !dec.is_done(),
            "decoder must not reach Done with fewer than 5 bytes"
        );
    }

    #[kani::proof]
    #[kani::unwind(33)]
    fn proof_decoder_full_consumption() {
        // Build a well-formed single-chunk body: "{hex_size}\r\n{data}\r\n0\r\n\r\n"
        let data_len: u8 = kani::any();
        kani::assume(data_len >= 1 && data_len <= 15);

        let hex_char = if data_len < 10 {
            b'0' + data_len
        } else {
            b'a' + (data_len - 10)
        };

        let mut buf = [0u8; 32];
        buf[0] = hex_char;
        buf[1] = b'\r';
        buf[2] = b'\n';
        for i in 0..(data_len as usize) {
            buf[3 + i] = b'x';
        }
        let off = 3 + data_len as usize;
        buf[off] = b'\r';
        buf[off + 1] = b'\n';
        buf[off + 2] = b'0';
        buf[off + 3] = b'\r';
        buf[off + 4] = b'\n';
        buf[off + 5] = b'\r';
        buf[off + 6] = b'\n';
        let total_len = off + 7;

        let mut dec = ChunkedDecoder::new();
        let progress = dec.feed(&buf[..total_len]);

        assert!(dec.is_done(), "well-formed input must complete");
        assert!(
            progress.accounting.wire_bytes == total_len as u64,
            "wire_bytes must equal total input length"
        );
    }

    #[kani::proof]
    #[kani::unwind(33)]
    fn proof_decoder_payload_accounting() {
        let len: usize = kani::any();
        kani::assume(len <= 32);
        let mut buf = [0u8; 32];
        for i in 0..len {
            buf[i] = kani::any();
        }

        let mut dec = ChunkedDecoder::new();
        let progress = dec.feed(&buf[..len]);

        assert!(
            progress.accounting.payload_bytes <= progress.accounting.wire_bytes,
            "payload_bytes must never exceed wire_bytes"
        );
    }

    #[kani::proof]
    fn proof_decoder_no_false_done() {
        // "0\r\n\r\n" embedded inside a 5-byte data chunk must not trigger Done
        // Layout: "5\r\n" (size line) + "0\r\n\r\n" (5 data bytes) + "\r\n" (post-data CRLF) + "0\r\n\r\n" (terminator)
        let input: [u8; 15] = *b"5\r\n0\r\n\r\n\r\n0\r\n\r\n";

        let mut dec = ChunkedDecoder::new();
        // Feed up to end of data + post-data CRLF
        let p1 = dec.feed(&input[..10]); // "5\r\n0\r\n\r\n\r\n"
        assert!(!dec.is_done(), "must not treat embedded 0\\r\\n\\r\\n as terminal");
        // Feed the real terminator "0\r\n\r\n"
        let p2 = dec.feed(&input[10..]);
        assert!(dec.is_done(), "must finish after real terminator");
        assert!(
            p1.accounting.payload_bytes + p2.accounting.payload_bytes == 5,
            "total payload must be 5"
        );
    }

    // ── Response framing proofs ──

    #[kani::proof]
    fn proof_head_always_bodyless() {
        let status_code: u16 = kani::any();
        kani::assume(status_code >= 100 && status_code <= 599);
        let has_chunked: bool = kani::any();
        let cl_val: usize = kani::any();
        kani::assume(cl_val <= 1_000_000);
        let has_cl: bool = kani::any();
        let content_length = if has_cl { Some(cl_val) } else { None };

        // expects_body = false simulates HEAD method
        let kind = classify_body_kind(false, status_code, has_chunked, content_length);
        assert!(
            matches!(kind, BodyKind::None),
            "HEAD must always produce BodyKind::None"
        );
    }

    #[kani::proof]
    fn proof_1xx_204_304_bodyless() {
        let status_code: u16 = kani::any();
        kani::assume(
            (status_code >= 100 && status_code <= 199)
                || status_code == 204
                || status_code == 304,
        );
        let has_chunked: bool = kani::any();
        let cl_val: usize = kani::any();
        kani::assume(cl_val <= 1_000_000);
        let has_cl: bool = kani::any();
        let content_length = if has_cl { Some(cl_val) } else { None };

        let kind = classify_body_kind(true, status_code, has_chunked, content_length);
        assert!(
            matches!(kind, BodyKind::None),
            "1xx/204/304 must produce BodyKind::None"
        );
    }

    #[kani::proof]
    fn proof_chunked_requires_te() {
        let status_code: u16 = kani::any();
        kani::assume(status_code >= 200 && status_code <= 599);
        kani::assume(status_code != 204 && status_code != 304);
        let has_chunked: bool = kani::any();
        let cl_val: usize = kani::any();
        kani::assume(cl_val <= 1_000_000);
        let has_cl: bool = kani::any();
        let content_length = if has_cl { Some(cl_val) } else { None };

        let kind = classify_body_kind(true, status_code, has_chunked, content_length);
        if matches!(kind, BodyKind::Chunked) {
            assert!(
                has_chunked,
                "Chunked body kind requires Transfer-Encoding: chunked"
            );
        }
    }

    #[kani::proof]
    fn proof_content_length_nonneg() {
        let status_code: u16 = kani::any();
        kani::assume(status_code >= 200 && status_code <= 599);
        kani::assume(status_code != 204 && status_code != 304);
        let cl_val: usize = kani::any();
        kani::assume(cl_val <= 1_000_000);

        let kind = classify_body_kind(true, status_code, false, Some(cl_val));
        if let BodyKind::Fixed(n) = kind {
            assert!(n == cl_val, "Fixed body length must match content-length header");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Shutdown, TcpListener};
    use std::thread;

    #[test]
    fn parses_content_length_response() {
        let parsed =
            parse_response_head(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello", true)
                .unwrap()
                .unwrap();

        assert_eq!(parsed.status_code, 200);
        assert_eq!(parsed.body_kind, BodyKind::Fixed(5));
        assert!(!parsed.connection_close);
    }

    #[test]
    fn treats_head_responses_as_bodyless() {
        let parsed = parse_response_head(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n", false)
            .unwrap()
            .unwrap();

        assert_eq!(parsed.body_kind, BodyKind::None);
    }

    #[test]
    fn runs_against_local_keep_alive_server() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = thread::spawn(move || {
            for stream in listener.incoming().flatten().take(2) {
                thread::spawn(move || handle_keep_alive_client(stream));
            }
        });

        let request_bytes = b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: keep-alive\r\n\r\n";
        let config = EngineConfig {
            worker_id: 7,
            remote_addr: addr,
            method: crate::loadtest::types::HttpMethod::GET,
            connections: 2,
            duration_seconds: 0,
            warmup_seconds: 0,
            mode: EngineMode::MaxThroughput,
            read_buffer_size: 4096,
        };

        let metrics = run_for_durations(
            &config,
            request_bytes,
            Duration::ZERO,
            Duration::from_millis(100),
        )
        .unwrap();

        assert!(
            metrics.request_count > 0,
            "expected at least one completed request"
        );
        assert_eq!(metrics.request_count, metrics.success_count);
        assert_eq!(metrics.error_count, 0);
        assert!(metrics.status_counts[200] > 0);

        server.join().unwrap();
    }

    #[test]
    fn parses_chunked_response() {
        let parsed = parse_response_head(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n",
            true,
        )
        .unwrap()
        .unwrap();

        assert_eq!(parsed.status_code, 200);
        assert_eq!(parsed.body_kind, BodyKind::Chunked);
    }

    #[test]
    fn rate_limited_runs_record_corrected_latency() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = thread::spawn(move || {
            for stream in listener.incoming().flatten().take(1) {
                thread::spawn(move || handle_keep_alive_client(stream));
            }
        });

        let request_bytes = b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: keep-alive\r\n\r\n";
        let config = EngineConfig {
            worker_id: 1,
            remote_addr: addr,
            method: crate::loadtest::types::HttpMethod::GET,
            connections: 1,
            duration_seconds: 0,
            warmup_seconds: 0,
            mode: EngineMode::RateLimited {
                requests_per_second: 20,
                latency_correction: true,
            },
            read_buffer_size: 4096,
        };

        let metrics = run_for_durations(
            &config,
            request_bytes,
            Duration::ZERO,
            Duration::from_millis(250),
        )
        .unwrap();

        assert!(metrics.request_count > 0, "expected measured requests");
        let corrected = metrics
            .latency_corrected
            .expect("corrected histogram should exist");
        assert_eq!(corrected.count(), metrics.request_count);

        server.join().unwrap();
    }

    #[test]
    fn chunked_decoder_simple_body() {
        let mut dec = ChunkedDecoder::new();
        let input = b"4\r\ndata\r\n0\r\n\r\n";
        let progress = dec.feed(input);
        assert!(dec.is_done());
        assert!(progress.complete);
        assert_eq!(progress.accounting.payload_bytes, 4);
        assert_eq!(progress.accounting.wire_bytes, input.len() as u64);
    }

    #[test]
    fn chunked_decoder_no_false_match_in_payload() {
        // Payload contains the bytes "0\r\n\r\n" but as data, not a terminal chunk
        let mut dec = ChunkedDecoder::new();
        let input = b"5\r\n0\r\n\r\n\r\n0\r\n\r\n";
        let progress = dec.feed(input);
        assert!(dec.is_done());
        assert!(progress.complete);
        assert_eq!(progress.accounting.payload_bytes, 5);
    }

    #[test]
    fn chunked_decoder_with_trailers() {
        let mut dec = ChunkedDecoder::new();
        let input = b"2\r\nok\r\n0\r\nTrailer: val\r\n\r\n";
        let progress = dec.feed(input);
        assert!(dec.is_done());
        assert!(progress.complete);
        assert_eq!(progress.accounting.payload_bytes, 2);
        assert_eq!(progress.accounting.wire_bytes, input.len() as u64);
    }

    #[test]
    fn chunked_decoder_split_across_feeds() {
        let mut dec = ChunkedDecoder::new();
        let full = b"4\r\ndata\r\n0\r\n\r\n";
        // Feed one byte at a time
        let mut total_wire = 0;
        let mut total_payload = 0;
        for &b in full.iter() {
            let progress = dec.feed(&[b]);
            total_wire += progress.accounting.wire_bytes as usize;
            total_payload += progress.accounting.payload_bytes as usize;
            if dec.is_done() {
                break;
            }
        }
        assert!(dec.is_done());
        assert_eq!(total_wire, full.len());
        assert_eq!(total_payload, 4);
    }

    #[test]
    fn chunked_decoder_multiple_chunks() {
        let mut dec = ChunkedDecoder::new();
        let input = b"3\r\nfoo\r\n3\r\nbar\r\n0\r\n\r\n";
        let progress = dec.feed(input);
        assert!(dec.is_done());
        assert_eq!(progress.accounting.payload_bytes, 6);
        assert_eq!(progress.accounting.wire_bytes, input.len() as u64);
    }

    #[test]
    fn chunked_decoder_chunk_extension() {
        let mut dec = ChunkedDecoder::new();
        let input = b"4;ext=val\r\ndata\r\n0\r\n\r\n";
        let progress = dec.feed(input);
        assert!(dec.is_done());
        assert_eq!(progress.accounting.payload_bytes, 4);
    }

    #[test]
    fn chunked_decoder_hex_uppercase() {
        let mut dec = ChunkedDecoder::new();
        let input = b"A\r\n0123456789\r\n0\r\n\r\n";
        let progress = dec.feed(input);
        assert!(dec.is_done());
        assert_eq!(progress.accounting.payload_bytes, 10);
    }

    #[test]
    fn chunked_decoder_incomplete() {
        let mut dec = ChunkedDecoder::new();
        let input = b"4\r\nda";
        let progress = dec.feed(input);
        assert!(!dec.is_done());
        assert!(!progress.complete);
        assert_eq!(progress.accounting.payload_bytes, 2);
    }

    #[test]
    fn runs_against_chunked_server() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = thread::spawn(move || {
            for stream in listener.incoming().flatten().take(2) {
                thread::spawn(move || handle_chunked_client(stream));
            }
        });

        let request_bytes = b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: keep-alive\r\n\r\n";
        let config = EngineConfig {
            worker_id: 0,
            remote_addr: addr,
            method: crate::loadtest::types::HttpMethod::GET,
            connections: 2,
            duration_seconds: 0,
            warmup_seconds: 0,
            mode: EngineMode::MaxThroughput,
            read_buffer_size: 4096,
        };

        let metrics = run_for_durations(
            &config,
            request_bytes,
            Duration::ZERO,
            Duration::from_millis(100),
        )
        .unwrap();

        assert!(
            metrics.request_count > 0,
            "expected at least one completed request against chunked server"
        );
        assert_eq!(
            metrics.error_count, 0,
            "chunked responses should not be errors"
        );
        assert!(metrics.status_counts[200] > 0);

        server.join().unwrap();
    }

    fn handle_chunked_client(mut stream: std::net::TcpStream) {
        let mut buf = [0u8; 4096];
        loop {
            let read = match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(read) => read,
                Err(_) => break,
            };

            if !buf[..read].windows(4).any(|window| window == b"\r\n\r\n") {
                continue;
            }

            let response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n2\r\nok\r\n0\r\n\r\n";
            if stream.write_all(response).is_err() {
                break;
            }
        }

        let _ = stream.shutdown(Shutdown::Both);
    }

    fn handle_keep_alive_client(mut stream: std::net::TcpStream) {
        let mut buf = [0u8; 4096];
        loop {
            let read = match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(read) => read,
                Err(_) => break,
            };

            if !buf[..read].windows(4).any(|window| window == b"\r\n\r\n") {
                continue;
            }

            if stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .is_err()
            {
                break;
            }
        }

        let _ = stream.shutdown(Shutdown::Both);
    }
}
