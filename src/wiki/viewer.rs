//! Read-only localhost viewer for compiled wiki artifacts.
//!
//! This module intentionally has no HTTP or Markdown dependency.  The wire
//! protocol is the small subset needed by a local viewer and all HTML is
//! generated from escaped text or trusted, locally generated markup.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const MAX_REQUEST_BYTES: usize = 32 * 1024;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 32 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 64;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

#[derive(Debug, Clone)]
pub struct ViewerConfig {
    pub project_root: PathBuf,
    pub bind: SocketAddr,
}

impl ViewerConfig {
    pub fn localhost(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        }
    }

    pub fn with_port(mut self, port: u16) -> Self {
        self.bind.set_port(port);
        self
    }
}

pub async fn serve(config: ViewerConfig) -> io::Result<()> {
    let viewer = WikiViewer::open(config.project_root.clone())?;
    viewer.serve(config).await
}

#[derive(Debug, Clone)]
pub struct WikiViewer {
    root: Arc<PathBuf>,
    manifest: Arc<Value>,
}

impl WikiViewer {
    pub fn open(project_root: impl Into<PathBuf>) -> io::Result<Self> {
        let project_root = project_root.into().canonicalize()?;
        let config = crate::wiki::config::WikiConfig::load(&project_root)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let state =
            crate::wiki::safety::validate_managed_directory(&project_root, &config.state_dir)
                .map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error))?;
        let output =
            crate::wiki::safety::validate_managed_directory(&project_root, &config.output_dir)
                .map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error))?;
        let manifest_path = [
            state.join("manifest.json"),
            output.join("manifest.json"),
            project_root.join(".klaar/wiki-manifest.json"),
        ]
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "wiki manifest not found"))?;
        let manifest_path = manifest_path.canonicalize()?;
        if !manifest_path.starts_with(&project_root) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "wiki manifest escaped the project root",
            ));
        }
        if manifest_path.metadata()?.len() > MAX_MANIFEST_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "wiki manifest exceeds viewer limit",
            ));
        }
        let manifest: Value = serde_json::from_slice(&fs::read(manifest_path)?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        Ok(Self {
            root: Arc::new(output),
            manifest: Arc::new(manifest),
        })
    }

    pub fn manifest(&self) -> &Value {
        &self.manifest
    }

    pub async fn serve(self, config: ViewerConfig) -> io::Result<()> {
        if !config.bind.ip().is_loopback() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "viewer bind address must be localhost",
            ));
        }
        let listener = TcpListener::bind(config.bind).await?;
        self.serve_listener(listener).await
    }

    pub async fn serve_listener(self, listener: TcpListener) -> io::Result<()> {
        let viewer = Arc::new(self);
        let connections = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
        loop {
            let (stream, _) = listener.accept().await?;
            let Ok(permit) = Arc::clone(&connections).try_acquire_owned() else {
                drop(stream);
                continue;
            };
            let viewer = Arc::clone(&viewer);
            tokio::spawn(async move {
                let _permit = permit;
                let _ = viewer.handle_connection(stream).await;
            });
        }
    }

    pub fn response(&self, method: &str, target: &str) -> Response {
        if method != "GET" && method != "HEAD" {
            return Response::text(405, "method not allowed");
        }
        let (path, query) = split_target(target);
        let route = match safe_route(path) {
            Ok(route) => route,
            Err(_) => return Response::text(400, "bad request"),
        };
        let lens = query_param(query, "lens").and_then(|value| safe_segment(&value).ok());
        match route.as_str() {
            "/" => Response::html(self.index(lens.as_deref())),
            "/search" => Response::html(self.search(query)),
            "/provenance" => Response::html(self.provenance()),
            "/freshness" => Response::html(self.freshness()),
            route if route.starts_with("/page/") => self.page(&route[6..], lens.as_deref()),
            _ => Response::text(404, "not found"),
        }
    }

    async fn handle_connection(&self, mut stream: TcpStream) -> io::Result<()> {
        let mut bytes = vec![0; MAX_REQUEST_BYTES];
        let count = tokio::time::timeout(REQUEST_TIMEOUT, stream.read(&mut bytes))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "request header timed out"))??;
        if count == MAX_REQUEST_BYTES && !bytes[..count].windows(4).any(|w| w == b"\r\n\r\n") {
            return write_response(&mut stream, Response::text(431, "request too large"), false)
                .await;
        }
        let request = std::str::from_utf8(&bytes[..count])
            .ok()
            .and_then(|value| {
                let mut lines = value.split("\r\n");
                let request_line = lines.next()?;
                let host = lines
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("host").then_some(value)
                    })
                    .map(str::trim)?;
                is_local_host(host).then_some(request_line)
            })
            .unwrap_or("");
        let mut fields = request.splitn(3, ' ');
        let method = fields.next().unwrap_or("");
        let target = fields.next().unwrap_or("");
        let version = fields.next().unwrap_or("");
        if version != "HTTP/1.1" || target.is_empty() || target.len() > 16 * 1024 {
            return write_response(&mut stream, Response::text(400, "bad request"), false).await;
        }
        let response = self.response(method, target);
        write_response(&mut stream, response, method == "HEAD").await
    }

    fn index(&self, lens: Option<&str>) -> String {
        let lenses = self.lenses();
        let active = lens
            .map(str::to_string)
            .or_else(|| lenses.first().cloned())
            .unwrap_or_else(|| "technical".to_string());
        let mut body = String::from("<h1>Compiled wiki</h1><p class=\"muted\">Read-only local artifact viewer</p><form action=\"/search\"><input name=\"q\" placeholder=\"Search local pages\"><button>Search</button></form><nav><a href=\"/provenance\">Provenance</a> <a href=\"/freshness\">Freshness</a></nav><h2>Pages</h2><ul>");
        for page in self.pages() {
            let _ = write!(
                body,
                "<li><a href=\"/page/{}?lens={}\">{}</a></li>",
                url_encode(&page.0),
                url_encode(&active),
                escape(&page.1)
            );
        }
        body.push_str("</ul>");
        self.document("Wiki", &body, &active)
    }

    fn page(&self, name: &str, lens: Option<&str>) -> Response {
        let relative = match safe_artifact_path(name) {
            Ok(value) => value,
            Err(_) => return Response::text(400, "bad path"),
        };
        let active = lens.unwrap_or("technical");
        let mut candidates = self
            .manifest
            .get("lens_renders")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|render| text(render, "page_id") == relative && text(render, "lens") == active)
            .filter_map(|render| safe_artifact_path(&text(render, "output_path")).ok())
            .collect::<Vec<_>>();
        candidates.extend([
            format!("{}.{}.md", relative, active),
            format!("{}/{}.md", relative, active),
            format!("{}.md", relative),
        ]);
        let path = candidates
            .iter()
            .map(|candidate| self.root.join(candidate))
            .find(|candidate| bounded_file(&self.root, candidate));
        let Some(path) = path else {
            return Response::text(404, "page not found");
        };
        let markdown = match fs::read_to_string(path) {
            Ok(value) => value,
            Err(_) => return Response::text(404, "page not found"),
        };
        let title = self
            .pages()
            .into_iter()
            .find(|(id, _)| id == name)
            .map(|(_, title)| title)
            .unwrap_or_else(|| name.replace('-', " "));
        let body = format!(
            "<p><a href=\"/?lens={}\">Back to index</a></p><article><h1>{}</h1>{}</article>",
            url_encode(active),
            escape(&title),
            render_markdown(&markdown)
        );
        Response::html(self.document(&title, &body, active))
    }

    fn search(&self, query: &str) -> String {
        let term = query_param(query, "q").unwrap_or_default();
        let lens = query_param(query, "lens").unwrap_or_else(|| "technical".to_string());
        let needle = term.to_ascii_lowercase();
        let mut body = format!("<h1>Search</h1><form action=\"/search\"><input type=hidden name=lens value=\"{}\"><input name=\"q\" value=\"{}\"><button>Search</button></form>", escape(&lens), escape(&term));
        if !needle.is_empty() {
            body.push_str("<ul>");
            for render in self
                .manifest
                .get("lens_renders")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|render| text(render, "lens") == lens)
            {
                let id = text(render, "page_id");
                let Some(relative) = safe_artifact_path(&text(render, "output_path")).ok() else {
                    continue;
                };
                let path = self.root.join(relative);
                if bounded_file(&self.root, &path)
                    && fs::read_to_string(path)
                        .is_ok_and(|text| text.to_ascii_lowercase().contains(&needle))
                {
                    let title = self
                        .pages()
                        .into_iter()
                        .find(|(page, _)| page == &id)
                        .map(|(_, title)| title)
                        .unwrap_or_else(|| id.clone());
                    let _ = write!(
                        body,
                        "<li><a href=\"/page/{}?lens={}\">{}</a></li>",
                        url_encode(&id),
                        url_encode(&lens),
                        escape(&title)
                    );
                }
            }
            body.push_str("</ul>");
        }
        self.document("Search", &body, &lens)
    }

    fn provenance(&self) -> String {
        let mut body = String::from(
            "<h1>Provenance</h1><p>Manifest evidence and generated render records.</p><ul>",
        );
        for page in self
            .manifest
            .get("canonical_pages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let id = text(page, "page_id");
            let evidence = page
                .get("evidence")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            let _ = write!(
                body,
                "<li><strong>{}</strong>: {} evidence item(s)</li>",
                escape(&id),
                evidence
            );
        }
        body.push_str("</ul>");
        self.document("Provenance", &body, "technical")
    }

    fn freshness(&self) -> String {
        let mut body = String::from("<h1>Freshness</h1><ul>");
        for page in self
            .manifest
            .get("canonical_pages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let id = text(page, "page_id");
            let stale = page
                .get("stale_reasons")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            let _ = write!(
                body,
                "<li><strong>{}</strong>: {}</li>",
                escape(&id),
                if stale == 0 {
                    "current".to_string()
                } else {
                    format!("stale ({} reason(s))", stale)
                }
            );
        }
        body.push_str("</ul>");
        self.document("Freshness", &body, "technical")
    }

    fn pages(&self) -> Vec<(String, String)> {
        self.manifest
            .get("canonical_pages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|page| {
                let id = text(page, "page_id");
                (!id.is_empty()).then(|| (id, text(page, "title")))
            })
            .collect()
    }
    fn lenses(&self) -> Vec<String> {
        self.manifest
            .get("lenses")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_else(|| vec!["technical".to_string()])
    }
    fn document(&self, title: &str, body: &str, active: &str) -> String {
        let mut lenses = String::from("<nav>Lens: ");
        for lens in self.lenses() {
            let _ = write!(
                lenses,
                "<a href=\"?lens={}\">{}</a> ",
                url_encode(&lens),
                escape(&lens)
            );
        }
        lenses.push_str("</nav>");
        format!("<!doctype html><html lang=en><head><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\"><title>{}</title><style>body{{max-width:58rem;margin:2rem auto;padding:0 1rem;font:16px/1.55 sans-serif;color:#17202a}}a{{color:#075985}}pre{{overflow:auto;background:#eef2f4;padding:1rem}}code{{font-family:monospace}}.muted{{color:#58636e}}nav{{margin:1rem 0}}input,button{{font:inherit;padding:.45rem}}</style></head><body data-lens=\"{}\">{}{}</body></html>", escape(title), escape(active), lenses, body)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
}
impl Response {
    fn html(body: String) -> Self {
        Self {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body,
        }
    }
    fn text(status: u16, body: &str) -> Self {
        Self {
            status,
            content_type: "text/plain; charset=utf-8",
            body: body.to_string(),
        }
    }
}

async fn write_response(stream: &mut TcpStream, response: Response, head: bool) -> io::Result<()> {
    let reason = match response.status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        431 => "Request Header Fields Too Large",
        _ => "Error",
    };
    let body = response.body.as_bytes();
    let mut output = format!("HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nContent-Security-Policy: {}\r\nX-Content-Type-Options: nosniff\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", response.status, reason, response.content_type, body.len(), CSP).into_bytes();
    if !head {
        output.extend_from_slice(body);
    }
    stream.write_all(&output).await
}

fn split_target(target: &str) -> (&str, &str) {
    target
        .split_once('?')
        .map_or((target, ""), |(path, query)| (path, query))
}
fn is_local_host(host: &str) -> bool {
    if let Some(value) = host.strip_prefix('[') {
        return value.split(']').next() == Some("::1");
    }
    host.split(':')
        .next()
        .is_some_and(|value| value.eq_ignore_ascii_case("localhost") || value == "127.0.0.1")
}
fn safe_route(path: &str) -> Result<String, ()> {
    if !path.starts_with('/') || path.contains('\\') {
        return Err(());
    }
    let decoded = percent_decode(path)?;
    if decoded.bytes().any(|b| b == 0 || b < 0x20) || decoded.split('/').any(|part| part == "..") {
        return Err(());
    }
    Ok(decoded)
}
fn safe_artifact_path(path: &str) -> Result<String, ()> {
    let path = percent_decode(path)?;
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || !path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'/' || b == b'_' || b == b'-' || b == b'.')
    {
        return Err(());
    }
    Ok(path)
}
fn safe_segment(value: &str) -> Result<String, ()> {
    let value = percent_decode(value)?;
    if value.is_empty()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
    {
        Err(())
    } else {
        Ok(value)
    }
}
fn percent_decode(input: &str) -> Result<String, ()> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(());
            }
            let hi = hex(bytes[index + 1])?;
            let lo = hex(bytes[index + 2])?;
            out.push(hi * 16 + lo);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).map_err(|_| ())
}
fn hex(byte: u8) -> Result<u8, ()> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(()),
    }
}
fn query_param(query: &str, wanted: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|part| part.split_once('='))
        .find(|(key, _)| *key == wanted)
        .and_then(|(_, value)| percent_decode(value).ok())
}
fn bounded_file(root: &Path, path: &Path) -> bool {
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    path.extension().is_some_and(|ext| ext == "md")
        && path
            .metadata()
            .ok()
            .is_some_and(|meta| meta.is_file() && meta.len() <= MAX_FILE_BYTES)
        && path
            .canonicalize()
            .ok()
            .is_some_and(|resolved| resolved.starts_with(&canonical_root))
}
fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn url_encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.' || b == b'/' {
                (b as char).to_string()
            } else {
                format!("%{:02X}", b)
            }
        })
        .collect()
}

fn render_markdown(markdown: &str) -> String {
    let mut output = String::new();
    let mut in_code = false;
    for line in markdown.lines() {
        let trimmed = line.trim_end();
        if trimmed.starts_with("```") {
            if in_code {
                output.push_str("</code></pre>");
            } else {
                output.push_str("<pre><code>");
            }
            in_code = !in_code;
            continue;
        }
        if in_code {
            output.push_str(&escape(trimmed));
            output.push('\n');
            continue;
        }
        let (prefix, tag) = if trimmed.starts_with("### ") {
            ("### ", "h3")
        } else if trimmed.starts_with("## ") {
            ("## ", "h2")
        } else if trimmed.starts_with("# ") {
            ("# ", "h1")
        } else {
            ("", "")
        };
        if !prefix.is_empty() {
            let _ = write!(
                output,
                "<{tag}>{}</{tag}>",
                inline(&trimmed[prefix.len()..])
            );
        } else if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            let _ = write!(output, "<p>&bull; {}</p>", inline(item));
        } else if !trimmed.is_empty() {
            let _ = write!(output, "<p>{}</p>", inline(trimmed));
        }
    }
    if in_code {
        output.push_str("</code></pre>");
    }
    output
}
fn inline(value: &str) -> String {
    let escaped = escape(value);
    let mut output = String::new();
    let mut rest = escaped.as_str();
    while let Some(start) = rest.find('[') {
        output.push_str(&rest[..start]);
        let Some(mid) = rest[start + 1..].find("](") else {
            output.push_str(&rest[start..]);
            return output;
        };
        let label_end = start + 1 + mid;
        let Some(end) = rest[label_end + 2..].find(')') else {
            output.push_str(&rest[start..]);
            return output;
        };
        let url_end = label_end + 2 + end;
        let label = &rest[start + 1..label_end];
        let url = &rest[label_end + 2..url_end];
        if url.starts_with('/') || url.starts_with("./") || url.starts_with("#") {
            let _ = write!(
                output,
                "<a href=\"{}\">{}</a>",
                url.replace('"', "%22"),
                label
            );
        } else {
            let _ = write!(output, "{}", label);
        }
        rest = &rest[url_end + 1..];
    }
    output.push_str(rest);
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn request(listener: std::net::SocketAddr, request: &str) -> String {
        let mut stream = TcpStream::connect(listener).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        stream.shutdown().await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    }
    #[test]
    fn paths_fail_closed() {
        assert!(safe_route("/page/%2e%2e/secret").is_err());
        assert!(safe_artifact_path("../secret").is_err());
        assert!(safe_artifact_path("a\\b").is_err());
        assert!(safe_artifact_path("docs/index").is_ok());
        assert!(is_local_host("LOCALHOST:8765"));
        assert!(is_local_host("[::1]:8765"));
        assert!(!is_local_host(""));
        assert!(!is_local_host("attacker.example"));
    }
    #[test]
    fn markdown_is_inert() {
        let html = render_markdown("# <script>alert(1)</script>\n\n[remote](https://example.test)");
        assert!(!html.contains("<script>"));
        assert!(!html.contains("https://"));
        assert!(html.contains("&lt;script&gt;"));
    }
    #[test]
    fn only_get_and_head_are_allowed() {
        let viewer = WikiViewer {
            root: Arc::new(PathBuf::from(".")),
            manifest: Arc::new(Value::Object(Default::default())),
        };
        assert_eq!(viewer.response("POST", "/").status, 405);
        assert_eq!(viewer.response("HEAD", "/").status, 200);
    }

    #[tokio::test]
    async fn socket_rejects_forged_hosts_and_traversal_with_security_headers() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("README.md"), "# Viewer\n").unwrap();
        crate::wiki::build::init(root.path()).unwrap();
        crate::wiki::build::build(root.path(), None).await.unwrap();
        let viewer = WikiViewer::open(root.path().to_path_buf()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(viewer.serve_listener(listener));

        let good = request(address, "GET / HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
        assert!(good.starts_with("HTTP/1.1 200"));
        assert!(good.contains("Content-Security-Policy: default-src 'none'"));
        assert!(good.contains("X-Content-Type-Options: nosniff"));
        assert!(good.contains("Cache-Control: no-store"));

        let forged = request(address, "GET / HTTP/1.1\r\nHost: attacker.example\r\n\r\n").await;
        assert!(forged.starts_with("HTTP/1.1 400"));

        let traversal = request(
            address,
            "GET /%2e%2e/README.md HTTP/1.1\r\nHost: localhost\r\n\r\n",
        )
        .await;
        assert!(traversal.starts_with("HTTP/1.1 400"));
        server.abort();
    }
}
