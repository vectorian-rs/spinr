#![allow(dead_code)]

use assert_cmd::Command;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

// ---------------------------------------------------------------------------
// TestServer — lightweight HTTP/1.1 server for integration tests
// ---------------------------------------------------------------------------

pub struct TestServer {
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl TestServer {
    /// Start a server that responds 200 OK with body "ok".
    pub fn start() -> Self {
        Self::start_with_response(200, "ok")
    }

    /// Start a server with a custom status code and body.
    pub fn start_with_response(status: u16, body: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind to ephemeral port");
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();
        let body = body.to_string();

        let handle = std::thread::spawn(move || {
            let response = format!(
                "HTTP/1.1 {} OK\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{}",
                status,
                body.len(),
                body
            );

            while !stop_clone.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let resp = response.clone();
                        let stop2 = stop_clone.clone();
                        std::thread::spawn(move || {
                            handle_connection(stream, &resp, &stop2);
                        });
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            port,
            stop,
            handle: Some(handle),
        }
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

fn handle_connection(mut stream: std::net::TcpStream, response: &str, stop: &AtomicBool) {
    stream
        .set_read_timeout(Some(std::time::Duration::from_millis(500)))
        .ok();
    let mut reader = BufReader::new(stream.try_clone().unwrap());

    while !stop.load(Ordering::Relaxed) {
        // Read request line
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return, // connection closed
            Err(_) => return,
            _ => {}
        }
        if line.trim().is_empty() {
            continue;
        }

        // Drain headers, track Content-Length
        let mut content_length: usize = 0;
        loop {
            let mut header_line = String::new();
            match reader.read_line(&mut header_line) {
                Ok(0) => return,
                Err(_) => return,
                _ => {}
            }
            if header_line.trim().is_empty() {
                break;
            }
            if let Some(val) = header_line
                .to_ascii_lowercase()
                .strip_prefix("content-length:")
            {
                content_length = val.trim().parse().unwrap_or(0);
            }
        }

        // Drain request body if present
        if content_length > 0 {
            let mut body = vec![0u8; content_length];
            if reader.read_exact(&mut body).is_err() {
                return;
            }
        }

        // Send response
        if stream.write_all(response.as_bytes()).is_err() {
            return;
        }
        if stream.flush().is_err() {
            return;
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Connect to unblock the accept loop
        let _ = std::net::TcpStream::connect(format!("127.0.0.1:{}", self.port));
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Build an `assert_cmd::Command` for the `spinr` binary.
pub fn spinr_cmd() -> Command {
    Command::cargo_bin("spinr").expect("spinr binary not found")
}

/// Write a TOML string into a temp directory and return its path.
pub fn write_bench_toml(dir: &Path, content: &str) -> PathBuf {
    let path = dir.join("bench.toml");
    std::fs::write(&path, content).expect("write bench TOML");
    path
}
