//! HTTP request tracer with detailed timing breakdown
//!
//! Traces each phase of an HTTP request: DNS, TCP, TLS, TTFB, transfer.

use crate::trace::types::{
    ConnectionInfo, HttpVersion, ResponseInfo, TimingInfo, TraceRequestArgs, TraceResult,
};
use anyhow::{Context, Result};
use hickory_resolver::TokioResolver;
use hickory_resolver::name_server::TokioConnectionProvider;
use http_body_util::{BodyExt, Empty};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use rustls_pki_types::ServerName;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

/// Trace an HTTP request with detailed timing
pub async fn trace_request(args: &TraceRequestArgs) -> Result<TraceResult> {
    let start = Instant::now();
    let mut timing = TimingInfo::default();

    // Parse URL
    let url = url::Url::parse(&args.url).context("Invalid URL")?;
    let scheme = url.scheme();
    let is_https = scheme == "https";
    let host = url.host_str().context("URL missing host")?.to_string();
    let port = url.port().unwrap_or(if is_https { 443 } else { 80 });
    let path = if url.path().is_empty() {
        "/".to_string()
    } else {
        let p = url.path().to_string();
        if let Some(q) = url.query() {
            format!("{}?{}", p, q)
        } else {
            p
        }
    };

    // Phase 1: DNS Resolution
    let dns_start = Instant::now();
    let resolver = TokioResolver::builder(TokioConnectionProvider::default())?.build();
    let response = resolver
        .lookup_ip(&host)
        .await
        .context("DNS lookup failed")?;
    let ip = response.iter().next().context("No IP addresses found")?;
    timing.dns_lookup_ms = dns_start.elapsed().as_millis() as u64;

    let addr = SocketAddr::new(ip, port);
    let remote_ip = ip.to_string();

    // Phase 2: TCP Connection
    let tcp_start = Instant::now();
    let tcp_stream = tokio::time::timeout(
        Duration::from_secs(args.timeout_secs),
        TcpStream::connect(addr),
    )
    .await
    .context("TCP connection timeout")?
    .context("TCP connection failed")?;
    timing.tcp_connect_ms = tcp_start.elapsed().as_millis() as u64;

    // Branch based on HTTP version
    let (response_info, connection_info, request_size, ttfb_ms, transfer_ms) =
        if args.http_version == HttpVersion::Http2 {
            // HTTP/2 requires HTTPS with ALPN
            if !is_https {
                anyhow::bail!("HTTP/2 requires HTTPS");
            }
            trace_http2(
                tcp_stream,
                &host,
                &path,
                &args.method,
                &args.headers,
                args.body.as_deref(),
                &remote_ip,
                &mut timing,
            )
            .await?
        } else if is_https {
            // HTTP/1.x over TLS
            trace_http1_tls(
                tcp_stream,
                &host,
                &path,
                &args.method,
                &args.headers,
                args.body.as_deref(),
                args.http_version,
                &remote_ip,
                &mut timing,
            )
            .await?
        } else {
            // Plain HTTP/1.x
            let (resp, req_size, ttfb, transfer) = send_request_and_read(
                tcp_stream,
                &host,
                &path,
                &args.method,
                &args.headers,
                args.body.as_deref(),
                args.http_version,
            )
            .await?;

            (
                resp,
                ConnectionInfo {
                    remote_ip,
                    protocol: args.http_version.to_string(),
                    tls_version: None,
                },
                req_size,
                ttfb,
                transfer,
            )
        };

    timing.time_to_first_byte_ms = ttfb_ms;
    timing.content_transfer_ms = transfer_ms;
    timing.total_ms = start.elapsed().as_millis() as u64;

    Ok(TraceResult {
        url: args.url.clone(),
        method: args.method.clone(),
        request_size,
        timing,
        response: response_info,
        connection: connection_info,
    })
}

/// Trace HTTP/1.x request over TLS
#[allow(clippy::too_many_arguments)]
async fn trace_http1_tls(
    tcp_stream: TcpStream,
    host: &str,
    path: &str,
    method: &str,
    headers: &HashMap<String, String>,
    body: Option<&str>,
    http_version: HttpVersion,
    remote_ip: &str,
    timing: &mut TimingInfo,
) -> Result<(ResponseInfo, ConnectionInfo, usize, u64, u64)> {
    let tls_start = Instant::now();

    // Setup TLS without ALPN (HTTP/1.x)
    let mut root_store = tokio_rustls::rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let config = tokio_rustls::rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    let connector = TlsConnector::from(Arc::new(config));
    let server_name = ServerName::try_from(host.to_string())
        .map_err(|_| anyhow::anyhow!("Invalid server name"))?;

    let tls_stream = connector
        .connect(server_name, tcp_stream)
        .await
        .context("TLS handshake failed")?;

    timing.tls_handshake_ms = tls_start.elapsed().as_millis() as u64;

    // Get TLS info
    let (_, conn) = tls_stream.get_ref();
    let tls_version = match conn.protocol_version() {
        Some(tokio_rustls::rustls::ProtocolVersion::TLSv1_3) => "TLSv1.3",
        Some(tokio_rustls::rustls::ProtocolVersion::TLSv1_2) => "TLSv1.2",
        _ => "Unknown",
    };

    // Send request and read response over TLS
    let (resp, req_size, ttfb, transfer) =
        send_request_and_read(tls_stream, host, path, method, headers, body, http_version).await?;

    Ok((
        resp,
        ConnectionInfo {
            remote_ip: remote_ip.to_string(),
            protocol: http_version.to_string(),
            tls_version: Some(tls_version.to_string()),
        },
        req_size,
        ttfb,
        transfer,
    ))
}

/// Trace HTTP/2 request over TLS with ALPN
#[allow(clippy::too_many_arguments)]
async fn trace_http2(
    tcp_stream: TcpStream,
    host: &str,
    path: &str,
    method: &str,
    headers: &HashMap<String, String>,
    body: Option<&str>,
    remote_ip: &str,
    timing: &mut TimingInfo,
) -> Result<(ResponseInfo, ConnectionInfo, usize, u64, u64)> {
    let tls_start = Instant::now();

    // Setup TLS with ALPN for HTTP/2
    let mut root_store = tokio_rustls::rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let mut config = tokio_rustls::rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    // Set ALPN protocols for HTTP/2
    config.alpn_protocols = vec![b"h2".to_vec()];

    let connector = TlsConnector::from(Arc::new(config));
    let server_name = ServerName::try_from(host.to_string())
        .map_err(|_| anyhow::anyhow!("Invalid server name"))?;

    let tls_stream = connector
        .connect(server_name, tcp_stream)
        .await
        .context("TLS handshake failed")?;

    timing.tls_handshake_ms = tls_start.elapsed().as_millis() as u64;

    // Verify ALPN negotiated HTTP/2
    let (_, conn) = tls_stream.get_ref();
    let alpn = conn.alpn_protocol();
    if alpn != Some(b"h2".as_slice()) {
        anyhow::bail!(
            "Server does not support HTTP/2 (ALPN: {:?})",
            alpn.map(|p| String::from_utf8_lossy(p).to_string())
        );
    }

    let tls_version = match conn.protocol_version() {
        Some(tokio_rustls::rustls::ProtocolVersion::TLSv1_3) => "TLSv1.3",
        Some(tokio_rustls::rustls::ProtocolVersion::TLSv1_2) => "TLSv1.2",
        _ => "Unknown",
    };

    // Wrap stream for hyper
    let io = TokioIo::new(tls_stream);

    // Build HTTP/2 request
    let authority = host.to_string();
    let uri = format!("https://{}{}", host, path);

    let req_body: Empty<Bytes> = Empty::new();
    let mut request = hyper::Request::builder()
        .method(method)
        .uri(&uri)
        .header("host", &authority)
        .header("user-agent", "spinr/0.1");

    for (key, value) in headers {
        request = request.header(key.as_str(), value.as_str());
    }

    // Note: HTTP/2 body support would require a different body type (e.g., Full<Bytes>)
    let _ = body;
    let request = request.body(req_body).context("Failed to build request")?;

    let request_size = uri.len()
        + headers
            .iter()
            .map(|(k, v)| k.len() + v.len() + 4)
            .sum::<usize>()
        + 50; // Approximate

    // Perform HTTP/2 handshake and send request
    let ttfb_start = Instant::now();

    let (mut sender, conn) =
        hyper::client::conn::http2::handshake(hyper_util::rt::TokioExecutor::new(), io)
            .await
            .context("HTTP/2 handshake failed")?;

    // Spawn connection driver
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            tracing::error!("HTTP/2 connection error: {}", e);
        }
    });

    // Send request
    let response = sender
        .send_request(request)
        .await
        .context("Failed to send HTTP/2 request")?;
    let ttfb_ms = ttfb_start.elapsed().as_millis() as u64;

    // Read response
    let transfer_start = Instant::now();
    let status = response.status().as_u16();
    let response_headers: HashMap<String, String> = response
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let headers_size = response_headers
        .iter()
        .map(|(k, v)| k.len() + v.len() + 4)
        .sum();

    // Read body
    let body_bytes = response
        .into_body()
        .collect()
        .await
        .context("Failed to read response body")?
        .to_bytes();
    let transfer_ms = transfer_start.elapsed().as_millis() as u64;

    let body_preview = String::from_utf8_lossy(&body_bytes)
        .chars()
        .take(500)
        .collect::<String>();

    Ok((
        ResponseInfo {
            status,
            redirect_url: if (300..400).contains(&status) {
                response_headers.get("location").cloned()
            } else {
                None
            },
            headers: response_headers,
            headers_size,
            body_size: body_bytes.len(),
            body_preview,
        },
        ConnectionInfo {
            remote_ip: remote_ip.to_string(),
            protocol: "HTTP/2".to_string(),
            tls_version: Some(tls_version.to_string()),
        },
        request_size,
        ttfb_ms,
        transfer_ms,
    ))
}

/// Send HTTP request and read response, returning timing info and byte counts
async fn send_request_and_read<S>(
    stream: S,
    host: &str,
    path: &str,
    method: &str,
    headers: &HashMap<String, String>,
    body: Option<&str>,
    http_version: HttpVersion,
) -> Result<(ResponseInfo, usize, u64, u64)>
where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
{
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);

    // HTTP version string for request line
    let version_str = match http_version {
        HttpVersion::Http10 => "HTTP/1.0",
        HttpVersion::Http11 => "HTTP/1.1",
        HttpVersion::Http2 => "HTTP/1.1",
    };

    // Build request
    let mut request = format!(
        "{} {} {}\r\nHost: {}\r\nConnection: close\r\nUser-Agent: spinr/0.1\r\n",
        method, path, version_str, host
    );

    for (key, value) in headers {
        request.push_str(&format!("{}: {}\r\n", key, value));
    }

    if let Some(body_content) = body {
        request.push_str(&format!("Content-Length: {}\r\n", body_content.len()));
    }

    request.push_str("\r\n");

    if let Some(body_content) = body {
        request.push_str(body_content);
    }

    let request_size = request.len();

    // Send request and measure TTFB
    let ttfb_start = Instant::now();
    writer
        .write_all(request.as_bytes())
        .await
        .context("Failed to send request")?;
    writer.flush().await.context("Failed to flush request")?;

    // Read status line (first byte of response)
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .await
        .context("Failed to read status line")?;
    let ttfb_ms = ttfb_start.elapsed().as_millis() as u64;

    // Track headers size (including status line)
    let mut headers_size = status_line.len();

    // Parse status code
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    // Read headers
    let transfer_start = Instant::now();
    let mut response_headers = HashMap::new();
    let mut content_length: Option<usize> = None;

    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        headers_size += line.len();
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            let key = key.trim().to_lowercase();
            let value = value.trim().to_string();
            if key == "content-length" {
                content_length = value.parse().ok();
            }
            response_headers.insert(key, value);
        }
    }

    // Read body
    let mut body = Vec::new();
    if let Some(len) = content_length {
        body.resize(len, 0);
        reader.read_exact(&mut body).await.ok();
    } else {
        // Read until EOF (chunked or connection close)
        reader.read_to_end(&mut body).await.ok();
    }

    let transfer_ms = transfer_start.elapsed().as_millis() as u64;

    // Create body preview
    let body_preview = String::from_utf8_lossy(&body)
        .chars()
        .take(500)
        .collect::<String>();

    Ok((
        ResponseInfo {
            status,
            redirect_url: if (300..400).contains(&status) {
                response_headers.get("location").cloned()
            } else {
                None
            },
            headers: response_headers,
            headers_size,
            body_size: body.len(),
            body_preview,
        },
        request_size,
        ttfb_ms,
        transfer_ms,
    ))
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn test_url_parsing() {
        let url = url::Url::parse("https://example.com/path?query=1").unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("example.com"));
        assert_eq!(url.port(), None);
        assert_eq!(url.path(), "/path");
        assert_eq!(url.query(), Some("query=1"));
    }
}
