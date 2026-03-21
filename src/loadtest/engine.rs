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
            total_bytes: 0,
        }
    }

    fn record_response(
        &mut self,
        status_code: Option<u16>,
        request_start: Instant,
        scheduled_start: Option<Instant>,
        body_len: u64,
        is_transport_error: bool,
    ) {
        self.request_count += 1;
        self.total_bytes += body_len;

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
        metrics.record_response(None, request_start, scheduled_start, 0, true);
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
                                                0,
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
                                                    total_len as u64,
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
                                        return handle_connection_failure(
                                            poll,
                                            connection,
                                            metrics,
                                            allow_reconnect,
                                        );
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
                                    total_body_len,
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

            let body_kind = if !expects_body || matches!(status_code, 100..=199 | 204 | 304) {
                BodyKind::None
            } else if chunked {
                BodyKind::Chunked
            } else {
                BodyKind::Fixed(content_length.unwrap_or(0))
            };

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
            ConnectionState::ReadingHead | ConnectionState::DrainingContentLength { .. } => {
                Interest::READABLE
            }
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

        self.state = ConnectionState::Writing;
        self.write_pos = 0;
        self.read_len = 0;
        self.request_start = Some(now);
        self.measure_request = now >= window.warmup_deadline && now < window.measurement_deadline;
        self.scheduled_start = match mode {
            EngineMode::MaxThroughput => None,
            EngineMode::RateLimited {
                latency_correction, ..
            } if *latency_correction => self.next_due_at(),
            EngineMode::RateLimited { .. } => None,
        };
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
            verify_body: false,
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
