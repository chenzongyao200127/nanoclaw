use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use nanoclaw_core::env::read_env_file;

use crate::runner::AuthMode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxySecrets {
    pub anthropic_api_key: Option<String>,
    pub claude_code_oauth_token: Option<String>,
    pub anthropic_auth_token: Option<String>,
    pub anthropic_base_url: Option<String>,
}

#[derive(Debug)]
pub struct CredentialProxyHandle {
    local_addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl CredentialProxyHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn shutdown(mut self) -> Result<(), String> {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| "proxy thread panicked".to_string())?;
        }
        Ok(())
    }
}

impl Drop for CredentialProxyHandle {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UpstreamUrl {
    base: String,
    host_header: String,
}

#[derive(Debug)]
struct ProxyState {
    mode: AuthMode,
    secrets: ProxySecrets,
    upstream: UpstreamUrl,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpResponse {
    status_code: u16,
    reason_phrase: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

pub fn load_proxy_secrets(project_root: &Path) -> ProxySecrets {
    let env_values = read_env_file(
        &project_root.join(".env"),
        &[
            "ANTHROPIC_API_KEY",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_BASE_URL",
        ],
    );

    ProxySecrets {
        anthropic_api_key: std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .or_else(|| env_values.get("ANTHROPIC_API_KEY").cloned()),
        claude_code_oauth_token: std::env::var("CLAUDE_CODE_OAUTH_TOKEN")
            .ok()
            .or_else(|| env_values.get("CLAUDE_CODE_OAUTH_TOKEN").cloned()),
        anthropic_auth_token: std::env::var("ANTHROPIC_AUTH_TOKEN")
            .ok()
            .or_else(|| env_values.get("ANTHROPIC_AUTH_TOKEN").cloned()),
        anthropic_base_url: std::env::var("ANTHROPIC_BASE_URL")
            .ok()
            .or_else(|| env_values.get("ANTHROPIC_BASE_URL").cloned()),
    }
}

pub fn start_credential_proxy(port: u16, host: &str) -> Result<CredentialProxyHandle, String> {
    let project_root = std::env::current_dir().map_err(|err| err.to_string())?;
    let secrets = load_proxy_secrets(&project_root);
    start_credential_proxy_with_secrets(port, host, secrets)
}

pub fn start_credential_proxy_with_secrets(
    port: u16,
    host: &str,
    secrets: ProxySecrets,
) -> Result<CredentialProxyHandle, String> {
    let mode = detect_auth_mode(&secrets);
    let upstream = parse_upstream_url(
        secrets
            .anthropic_base_url
            .as_deref()
            .unwrap_or("https://api.anthropic.com"),
    )?;

    let listener = TcpListener::bind((host, port)).map_err(|err| err.to_string())?;
    listener
        .set_nonblocking(true)
        .map_err(|err| err.to_string())?;
    let local_addr = listener.local_addr().map_err(|err| err.to_string())?;

    let shutdown = Arc::new(AtomicBool::new(false));
    let worker_shutdown = shutdown.clone();
    let state = Arc::new(ProxyState {
        mode,
        secrets,
        upstream,
    });

    let worker = thread::spawn(move || {
        while !worker_shutdown.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let state = state.clone();
                    thread::spawn(move || {
                        let _ = handle_connection(stream, &state);
                    });
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
    });

    Ok(CredentialProxyHandle {
        local_addr,
        shutdown,
        worker: Some(worker),
    })
}

pub fn detect_auth_mode(secrets: &ProxySecrets) -> AuthMode {
    if secrets
        .anthropic_api_key
        .as_ref()
        .is_some_and(|value| !value.is_empty())
    {
        AuthMode::ApiKey
    } else {
        AuthMode::OAuth
    }
}

pub fn rewrite_headers(
    incoming: &HashMap<String, String>,
    mode: AuthMode,
    secrets: &ProxySecrets,
    upstream_host: &str,
    body_len: usize,
) -> HashMap<String, String> {
    let mut headers = incoming.clone();
    headers.insert("host".to_string(), upstream_host.to_string());
    headers.insert("content-length".to_string(), body_len.to_string());

    headers.remove("connection");
    headers.remove("keep-alive");
    headers.remove("transfer-encoding");

    match mode {
        AuthMode::ApiKey => {
            headers.remove("x-api-key");
            if let Some(key) = &secrets.anthropic_api_key {
                headers.insert("x-api-key".to_string(), key.clone());
            }
        }
        AuthMode::OAuth => {
            if headers.contains_key("authorization") {
                headers.remove("authorization");
                let token = secrets
                    .claude_code_oauth_token
                    .clone()
                    .or_else(|| secrets.anthropic_auth_token.clone());
                if let Some(token) = token {
                    headers.insert("authorization".to_string(), format!("Bearer {token}"));
                }
            }
        }
    }

    headers
}

fn handle_connection(mut stream: TcpStream, state: &ProxyState) -> Result<(), String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|err| err.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|err| err.to_string())?;

    let request = read_http_request(&mut stream)?;
    let headers = rewrite_headers(
        &request.headers,
        state.mode,
        &state.secrets,
        &state.upstream.host_header,
        request.body.len(),
    );

    match forward_with_curl(
        &request.method,
        &join_url(&state.upstream.base, &request.path),
        &headers,
        &request.body,
    ) {
        Ok(response) => write_http_response(&mut stream, &response),
        Err(_) => write_bad_gateway(&mut stream),
    }
}

fn write_bad_gateway(stream: &mut TcpStream) -> Result<(), String> {
    write_http_response(
        stream,
        &HttpResponse {
            status_code: 502,
            reason_phrase: "Bad Gateway".to_string(),
            headers: vec![
                ("content-length".to_string(), "11".to_string()),
                (
                    "content-type".to_string(),
                    "text/plain; charset=utf-8".to_string(),
                ),
            ],
            body: b"Bad Gateway".to_vec(),
        },
    )
}

fn write_http_response(stream: &mut TcpStream, response: &HttpResponse) -> Result<(), String> {
    write!(
        stream,
        "HTTP/1.1 {} {}\r\n",
        response.status_code, response.reason_phrase
    )
    .map_err(|err| err.to_string())?;
    for (key, value) in &response.headers {
        write!(stream, "{key}: {value}\r\n").map_err(|err| err.to_string())?;
    }
    write!(stream, "\r\n").map_err(|err| err.to_string())?;
    stream.write_all(&response.body).map_err(|err| err.to_string())?;
    stream.flush().map_err(|err| err.to_string())
}

fn read_http_request(stream: &mut TcpStream) -> Result<HttpRequest, String> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut chunk).map_err(|err| err.to_string())?;
        if read == 0 {
            return Err("unexpected EOF while reading request".to_string());
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(end) = find_header_end(&buffer) {
            break end;
        }
        if buffer.len() > 1024 * 1024 {
            return Err("request headers too large".to_string());
        }
    };

    let headers_text = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = headers_text.split("\r\n");
    let request_line = lines.next().ok_or_else(|| "missing request line".to_string())?;
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts
        .next()
        .ok_or_else(|| "missing request method".to_string())?
        .to_string();
    let path = request_parts
        .next()
        .ok_or_else(|| "missing request path".to_string())?
        .to_string();

    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
    }

    let body_start = header_end + 4;
    let body_len = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buffer[body_start..].to_vec();
    while body.len() < body_len {
        let read = stream.read(&mut chunk).map_err(|err| err.to_string())?;
        if read == 0 {
            return Err("unexpected EOF while reading request body".to_string());
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(body_len);

    Ok(HttpRequest {
        method,
        path,
        headers,
        body,
    })
}

fn forward_with_curl(
    method: &str,
    url: &str,
    headers: &HashMap<String, String>,
    body: &[u8],
) -> Result<HttpResponse, String> {
    let mut command = Command::new("curl");
    command
        .arg("-sS")
        .arg("--include")
        .arg("--http1.1")
        .arg("-X")
        .arg(method)
        .arg(url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    for (key, value) in headers {
        command.arg("-H").arg(format!("{key}: {value}"));
    }
    if !body.is_empty() {
        command.arg("--data-binary").arg("@-");
    }

    let mut child = command.spawn().map_err(|err| err.to_string())?;
    if !body.is_empty() {
        child
            .stdin
            .as_mut()
            .ok_or_else(|| "missing curl stdin".to_string())?
            .write_all(body)
            .map_err(|err| err.to_string())?;
    }

    let output = child.wait_with_output().map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    parse_curl_response(&output.stdout)
}

fn parse_curl_response(stdout: &[u8]) -> Result<HttpResponse, String> {
    let header_end = find_header_end(stdout).ok_or_else(|| "missing response headers".to_string())?;
    let header_text = String::from_utf8_lossy(&stdout[..header_end]).to_string();
    let body = stdout[header_end + 4..].to_vec();

    let mut lines = header_text.split("\r\n");
    let status_line = lines.next().ok_or_else(|| "missing response status".to_string())?;
    let mut status_parts = status_line.splitn(3, ' ');
    let _http_version = status_parts.next();
    let status_code = status_parts
        .next()
        .ok_or_else(|| "missing status code".to_string())?
        .parse::<u16>()
        .map_err(|err| err.to_string())?;
    let reason_phrase = status_parts.next().unwrap_or("OK").to_string();

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }

    Ok(HttpResponse {
        status_code,
        reason_phrase,
        headers,
        body,
    })
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_upstream_url(raw: &str) -> Result<UpstreamUrl, String> {
    let (scheme, remainder) = raw
        .split_once("://")
        .ok_or_else(|| format!("invalid upstream URL: {raw}"))?;
    if scheme != "http" && scheme != "https" {
        return Err(format!("unsupported upstream scheme: {scheme}"));
    }

    let authority = remainder
        .split('/')
        .next()
        .ok_or_else(|| format!("invalid upstream URL: {raw}"))?;
    let base = format!("{scheme}://{}", remainder.trim_end_matches('/'));
    Ok(UpstreamUrl {
        base,
        host_header: authority.to_string(),
    })
}

fn join_url(base: &str, path: &str) -> String {
    if path.starts_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn secrets() -> ProxySecrets {
        ProxySecrets {
            anthropic_api_key: Some("sk-ant-real-key".to_string()),
            claude_code_oauth_token: Some("real-oauth-token".to_string()),
            anthropic_auth_token: None,
            anthropic_base_url: Some("http://127.0.0.1:3000".to_string()),
        }
    }

    fn read_raw_response(port: u16, request: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        stream.write_all(request.as_bytes()).expect("request");
        stream.shutdown(std::net::Shutdown::Write).expect("shutdown");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("response");
        response
    }

    fn start_upstream_server(
        response_body: &'static str,
        captured_headers: Arc<Mutex<HashMap<String, String>>>,
    ) -> (SocketAddr, JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let addr = listener.local_addr().expect("addr");
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let request = read_http_request(&mut stream).expect("request");
            *captured_headers.lock().expect("headers") = request.headers;

            let body = response_body.as_bytes();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                response_body
            )
            .expect("response");
        });
        (addr, worker)
    }

    #[test]
    fn detects_api_key_mode() {
        assert_eq!(detect_auth_mode(&secrets()), AuthMode::ApiKey);
        assert_eq!(
            detect_auth_mode(&ProxySecrets {
                anthropic_api_key: None,
                ..secrets()
            }),
            AuthMode::OAuth
        );
    }

    #[test]
    fn api_key_mode_injects_real_key() {
        let headers = rewrite_headers(
            &HashMap::from([
                ("content-type".to_string(), "application/json".to_string()),
                ("x-api-key".to_string(), "placeholder".to_string()),
            ]),
            AuthMode::ApiKey,
            &secrets(),
            "api.anthropic.com",
            2,
        );
        assert_eq!(
            headers.get("x-api-key").map(String::as_str),
            Some("sk-ant-real-key")
        );
    }

    #[test]
    fn oauth_mode_replaces_authorization_when_present() {
        let headers = rewrite_headers(
            &HashMap::from([
                ("authorization".to_string(), "Bearer placeholder".to_string()),
                ("content-type".to_string(), "application/json".to_string()),
            ]),
            AuthMode::OAuth,
            &ProxySecrets {
                anthropic_api_key: None,
                ..secrets()
            },
            "api.anthropic.com",
            2,
        );
        assert_eq!(
            headers.get("authorization").map(String::as_str),
            Some("Bearer real-oauth-token")
        );
    }

    #[test]
    fn oauth_mode_leaves_authorization_absent_when_not_sent() {
        let headers = rewrite_headers(
            &HashMap::from([("x-api-key".to_string(), "temp-key".to_string())]),
            AuthMode::OAuth,
            &ProxySecrets {
                anthropic_api_key: None,
                ..secrets()
            },
            "api.anthropic.com",
            2,
        );
        assert_eq!(headers.get("x-api-key").map(String::as_str), Some("temp-key"));
        assert!(!headers.contains_key("authorization"));
    }

    #[test]
    fn strips_hop_by_hop_headers() {
        let headers = rewrite_headers(
            &HashMap::from([
                ("connection".to_string(), "keep-alive".to_string()),
                ("keep-alive".to_string(), "timeout=5".to_string()),
                ("transfer-encoding".to_string(), "chunked".to_string()),
            ]),
            AuthMode::ApiKey,
            &secrets(),
            "api.anthropic.com",
            2,
        );
        assert!(!headers.contains_key("connection"));
        assert!(!headers.contains_key("keep-alive"));
        assert!(!headers.contains_key("transfer-encoding"));
    }

    #[test]
    fn proxy_forwards_request_with_api_key_injection() {
        let captured_headers = Arc::new(Mutex::new(HashMap::new()));
        let (upstream_addr, upstream_worker) =
            start_upstream_server("{\"ok\":true}", captured_headers.clone());

        let proxy = start_credential_proxy_with_secrets(
            0,
            "127.0.0.1",
            ProxySecrets {
                anthropic_base_url: Some(format!("http://{upstream_addr}")),
                ..secrets()
            },
        )
        .expect("proxy");

        let response = read_raw_response(
            proxy.local_addr().port(),
            "POST /v1/messages HTTP/1.1\r\nhost: 127.0.0.1\r\ncontent-type: application/json\r\ncontent-length: 2\r\nx-api-key: placeholder\r\n\r\n{}",
        );

        proxy.shutdown().expect("shutdown");
        upstream_worker.join().expect("upstream");

        assert!(response.contains("HTTP/1.1 200 OK"));
        assert!(response.contains("{\"ok\":true}"));
        assert_eq!(
            captured_headers
                .lock()
                .expect("headers")
                .get("x-api-key")
                .map(String::as_str),
            Some("sk-ant-real-key")
        );
    }

    #[test]
    fn proxy_rewrites_oauth_authorization_header() {
        let captured_headers = Arc::new(Mutex::new(HashMap::new()));
        let (upstream_addr, upstream_worker) =
            start_upstream_server("{\"ok\":true}", captured_headers.clone());

        let proxy = start_credential_proxy_with_secrets(
            0,
            "127.0.0.1",
            ProxySecrets {
                anthropic_api_key: None,
                anthropic_base_url: Some(format!("http://{upstream_addr}")),
                ..secrets()
            },
        )
        .expect("proxy");

        let response = read_raw_response(
            proxy.local_addr().port(),
            "POST /api/oauth/claude_cli/create_api_key HTTP/1.1\r\nhost: 127.0.0.1\r\ncontent-type: application/json\r\ncontent-length: 2\r\nauthorization: Bearer placeholder\r\n\r\n{}",
        );

        proxy.shutdown().expect("shutdown");
        upstream_worker.join().expect("upstream");

        assert!(response.contains("HTTP/1.1 200 OK"));
        assert_eq!(
            captured_headers
                .lock()
                .expect("headers")
                .get("authorization")
                .map(String::as_str),
            Some("Bearer real-oauth-token")
        );
    }

    #[test]
    fn proxy_returns_bad_gateway_when_upstream_is_unreachable() {
        let proxy = start_credential_proxy_with_secrets(
            0,
            "127.0.0.1",
            ProxySecrets {
                anthropic_base_url: Some("http://127.0.0.1:59999".to_string()),
                ..secrets()
            },
        )
        .expect("proxy");

        let response = read_raw_response(
            proxy.local_addr().port(),
            "POST /v1/messages HTTP/1.1\r\nhost: 127.0.0.1\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\n{}",
        );

        proxy.shutdown().expect("shutdown");
        assert!(response.contains("HTTP/1.1 502 Bad Gateway"));
        assert!(response.ends_with("Bad Gateway"));
    }
}
