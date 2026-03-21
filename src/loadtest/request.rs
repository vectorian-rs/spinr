//! Build raw HTTP/1.1 request bytes once and reuse them across connections.

use crate::loadtest::types::HttpMethod;
use std::collections::HashMap;
use thiserror::Error;
use url::Url;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BuildRequestError {
    #[error("target URL must use http://")]
    UnsupportedScheme,
    #[error("target URL must include a host")]
    MissingHost,
    #[error("fragments are not supported in HTTP request targets")]
    FragmentNotSupported,
    #[error("conflicting Content-Length header for request body")]
    ConflictingContentLength,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedRequest {
    pub remote_addr_authority: String,
    pub bytes: Box<[u8]>,
}

pub fn build_request_bytes(
    target_url: &str,
    method: HttpMethod,
    headers: &HashMap<String, String>,
    body: Option<&str>,
) -> Result<PreparedRequest, BuildRequestError> {
    let url = Url::parse(target_url).map_err(|_| BuildRequestError::UnsupportedScheme)?;
    if url.scheme() != "http" {
        return Err(BuildRequestError::UnsupportedScheme);
    }
    if url.fragment().is_some() {
        return Err(BuildRequestError::FragmentNotSupported);
    }

    url.host_str().ok_or(BuildRequestError::MissingHost)?;
    let authority = authority_string(&url);
    let path_and_query = if url[url::Position::BeforePath..].is_empty() {
        "/"
    } else {
        &url[url::Position::BeforePath..]
    };

    let mut request = Vec::with_capacity(256 + body.map_or(0, str::len));
    request.extend_from_slice(method.wire_name().as_bytes());
    request.extend_from_slice(b" ");
    request.extend_from_slice(path_and_query.as_bytes());
    request.extend_from_slice(b" HTTP/1.1\r\n");

    let has_host = has_header(headers, "host");
    let has_connection = has_header(headers, "connection");
    let has_content_length = has_header(headers, "content-length");

    if !has_host {
        request.extend_from_slice(b"Host: ");
        request.extend_from_slice(authority.as_bytes());
        request.extend_from_slice(b"\r\n");
    }

    if !has_connection {
        request.extend_from_slice(b"Connection: keep-alive\r\n");
    }

    if let Some(body) = body {
        if has_content_length {
            let header_len =
                parse_content_length(headers).ok_or(BuildRequestError::ConflictingContentLength)?;
            if header_len != body.len() {
                return Err(BuildRequestError::ConflictingContentLength);
            }
        } else {
            request.extend_from_slice(b"Content-Length: ");
            request.extend_from_slice(body.len().to_string().as_bytes());
            request.extend_from_slice(b"\r\n");
        }
    }

    append_user_headers(&mut request, headers);
    request.extend_from_slice(b"\r\n");

    if let Some(body) = body {
        request.extend_from_slice(body.as_bytes());
    }

    Ok(PreparedRequest {
        remote_addr_authority: authority,
        bytes: request.into_boxed_slice(),
    })
}

fn append_user_headers(request: &mut Vec<u8>, headers: &HashMap<String, String>) {
    let mut entries: Vec<_> = headers.iter().collect();
    entries.sort_by(|(left, _), (right, _)| left.cmp(right));

    for (name, value) in entries {
        request.extend_from_slice(name.as_bytes());
        request.extend_from_slice(b": ");
        request.extend_from_slice(value.as_bytes());
        request.extend_from_slice(b"\r\n");
    }
}

fn has_header(headers: &HashMap<String, String>, name: &str) -> bool {
    headers.keys().any(|key| key.eq_ignore_ascii_case(name))
}

fn parse_content_length(headers: &HashMap<String, String>) -> Option<usize> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse().ok())
}

fn authority_string(url: &Url) -> String {
    // url::Url::host_str() already includes brackets for IPv6 (e.g. "[::1]").
    let host = url.host_str().unwrap();
    match url.port() {
        Some(port) if port != default_port(url.scheme()) => {
            format!("{}:{}", host, port)
        }
        _ => host.to_string(),
    }
}

fn default_port(scheme: &str) -> u16 {
    match scheme {
        "http" => 80,
        _ => 0,
    }
}

trait HttpMethodExt {
    fn wire_name(self) -> &'static str;
}

impl HttpMethodExt for HttpMethod {
    fn wire_name(self) -> &'static str {
        match self {
            HttpMethod::GET => "GET",
            HttpMethod::POST => "POST",
            HttpMethod::PUT => "PUT",
            HttpMethod::DELETE => "DELETE",
            HttpMethod::PATCH => "PATCH",
            HttpMethod::HEAD => "HEAD",
            HttpMethod::OPTIONS => "OPTIONS",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_get_request_with_derived_host_and_keep_alive() {
        let prepared = build_request_bytes(
            "http://example.com:8080/path?q=1",
            HttpMethod::GET,
            &HashMap::new(),
            None,
        )
        .unwrap();

        let request = String::from_utf8(prepared.bytes.into_vec()).unwrap();
        assert!(request.starts_with("GET /path?q=1 HTTP/1.1\r\n"));
        assert!(request.contains("\r\nHost: example.com:8080\r\n"));
        assert!(request.contains("\r\nConnection: keep-alive\r\n"));
        assert!(request.ends_with("\r\n\r\n"));
    }

    #[test]
    fn preserves_explicit_host_and_connection_headers() {
        let headers = HashMap::from([
            ("Host".to_string(), "bench.internal".to_string()),
            ("Connection".to_string(), "close".to_string()),
            ("X-Test".to_string(), "1".to_string()),
        ]);

        let prepared =
            build_request_bytes("http://example.com/", HttpMethod::GET, &headers, None).unwrap();
        let request = String::from_utf8(prepared.bytes.into_vec()).unwrap();

        assert!(request.contains("\r\nHost: bench.internal\r\n"));
        assert!(request.contains("\r\nConnection: close\r\n"));
        assert!(request.contains("\r\nX-Test: 1\r\n"));
        assert_eq!(request.matches("Host:").count(), 1);
        assert_eq!(request.matches("Connection:").count(), 1);
    }

    #[test]
    fn adds_content_length_and_body() {
        let prepared = build_request_bytes(
            "http://example.com/upload",
            HttpMethod::POST,
            &HashMap::from([("Content-Type".to_string(), "application/json".to_string())]),
            Some(r#"{"ok":true}"#),
        )
        .unwrap();

        let request = String::from_utf8(prepared.bytes.into_vec()).unwrap();
        assert!(request.contains("\r\nContent-Length: 11\r\n"));
        assert!(request.contains("\r\nContent-Type: application/json\r\n"));
        assert!(request.ends_with("\r\n\r\n{\"ok\":true}"));
    }

    #[test]
    fn rejects_conflicting_content_length() {
        let headers = HashMap::from([("Content-Length".to_string(), "9".to_string())]);
        let err = build_request_bytes(
            "http://example.com/upload",
            HttpMethod::POST,
            &headers,
            Some("payload"),
        )
        .unwrap_err();

        assert_eq!(err, BuildRequestError::ConflictingContentLength);
    }

    #[test]
    fn rejects_non_http_scheme() {
        let err = build_request_bytes(
            "https://example.com",
            HttpMethod::GET,
            &HashMap::new(),
            None,
        )
        .unwrap_err();
        assert_eq!(err, BuildRequestError::UnsupportedScheme);
    }

    #[test]
    fn ipv6_with_non_default_port() {
        let prepared = build_request_bytes(
            "http://[::1]:8080/path",
            HttpMethod::GET,
            &HashMap::new(),
            None,
        )
        .unwrap();

        let request = String::from_utf8(prepared.bytes.into_vec()).unwrap();
        assert!(request.contains("\r\nHost: [::1]:8080\r\n"));
        assert_eq!(prepared.remote_addr_authority, "[::1]:8080");
    }

    #[test]
    fn ipv6_with_default_port() {
        let prepared =
            build_request_bytes("http://[::1]/path", HttpMethod::GET, &HashMap::new(), None)
                .unwrap();

        let request = String::from_utf8(prepared.bytes.into_vec()).unwrap();
        assert!(request.contains("\r\nHost: [::1]\r\n"));
        assert_eq!(prepared.remote_addr_authority, "[::1]");
    }
}
