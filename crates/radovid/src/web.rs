//! Static files on the control listener's own origin: a directory served read-only (`[web]
//! static_dir`), so a page and the service it talks to share one URL. What comes in is untrusted:
//! the request head is read bounded and strictly, only `GET` and `HEAD` are answered, a path is
//! limited to plain file names and must resolve, symbolic links followed, to a file inside the
//! directory, and nothing is ever listed.

use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The longest request head read; a client that sends more is cut off.
pub const MAX_HEAD_LEN: usize = 8 * 1024;
/// Time to take in each chunk of a file the peer is slow to read.
const WRITE_TIMEOUT: Duration = Duration::from_secs(20);
/// File bytes sent per write.
const CHUNK: usize = 64 * 1024;

/// What a request head says, of what is used here.
#[derive(Debug, PartialEq, Eq)]
pub struct Head {
    pub method: String,
    pub path: String,
    /// The head has `Upgrade: websocket`.
    pub upgrade: bool,
    pub accept_encoding: String,
    pub if_none_match: Option<String>,
}

/// Reads a request head out of `bytes` (up to and with its blank line); none if it is not
/// complete or not a plain HTTP/1.1 request.
pub fn parse(bytes: &[u8]) -> Option<Head> {
    let end = bytes.windows(4).position(|w| w == b"\r\n\r\n")?;
    let text = std::str::from_utf8(&bytes[..end]).ok()?;
    let mut lines = text.split("\r\n");
    let mut request = lines.next()?.split(' ');
    let (method, target, version) = (request.next()?, request.next()?, request.next()?);
    if request.next().is_some() || !version.starts_with("HTTP/1.") || method.len() > 8 {
        return None;
    }
    let mut head = Head {
        method: method.to_owned(),
        path: target.to_owned(),
        upgrade: false,
        accept_encoding: String::new(),
        if_none_match: None,
    };
    for line in lines {
        let (name, value) = line.split_once(':')?;
        let value = value.trim();
        match name.to_ascii_lowercase().as_str() {
            "upgrade" => head.upgrade = value.eq_ignore_ascii_case("websocket"),
            "accept-encoding" => head.accept_encoding = value.to_ascii_lowercase(),
            "if-none-match" => head.if_none_match = Some(value.to_owned()),
            _ => {}
        }
    }
    Some(head)
}

/// The file a request path names, relative to the root: plain components of letters, digits and
/// `._~-` only, none starting with a dot, none empty (`/` is `index.html`). Anything else (a
/// percent escape, a backslash, `..`) names nothing.
fn relative(path: &str) -> Option<PathBuf> {
    let path = path.split(['?', '#']).next()?;
    let path = path.strip_prefix('/')?;
    if path.is_empty() {
        return Some(PathBuf::from("index.html"));
    }
    let mut out = PathBuf::new();
    for part in path.split('/') {
        let plain = !part.is_empty()
            && !part.starts_with('.')
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'-'));
        if !plain {
            return None;
        }
        out.push(part);
    }
    Some(out)
}

/// The media type of a file name, by its extension.
fn media_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("wasm") => "application/wasm",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("ktx2") => "image/ktx2",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("txt") => "text/plain; charset=utf-8",
        Some("ogg") => "audio/ogg",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        _ => "application/octet-stream",
    }
}

/// A file inside `root` (both canonical), if `relative` resolves to one.
async fn resolve(root: &Path, relative: &Path) -> Option<(PathBuf, std::fs::Metadata)> {
    let found = tokio::fs::canonicalize(root.join(relative)).await.ok()?;
    if !found.starts_with(root) {
        return None;
    }
    let meta = tokio::fs::metadata(&found).await.ok()?;
    meta.is_file().then_some((found, meta))
}

fn etag(meta: &std::fs::Metadata) -> String {
    let secs = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    format!("\"{:x}-{secs:x}\"", meta.len())
}

async fn send<W: AsyncWrite + Unpin>(out: &mut W, bytes: &[u8]) -> std::io::Result<()> {
    tokio::time::timeout(WRITE_TIMEOUT, out.write_all(bytes))
        .await
        .map_err(|_| std::io::Error::other("the peer reads too slowly"))?
}

async fn plain<W: AsyncWrite + Unpin>(
    out: &mut W,
    status: &str,
    extra: &str,
) -> std::io::Result<()> {
    let body = status.as_bytes();
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\n\
         X-Content-Type-Options: nosniff\r\nConnection: close\r\n{extra}\r\n",
        body.len()
    );
    send(out, head.as_bytes()).await?;
    send(out, body).await
}

/// Answers one request from the files under `root` and leaves the connection to be closed.
pub async fn serve<W: AsyncWrite + Unpin>(
    out: &mut W,
    root: &Path,
    head: &Head,
) -> std::io::Result<()> {
    let sending_body = match head.method.as_str() {
        "GET" => true,
        "HEAD" => false,
        _ => return plain(out, "405 Method Not Allowed", "Allow: GET, HEAD\r\n").await,
    };
    let Ok(root) = tokio::fs::canonicalize(root).await else {
        return plain(out, "404 Not Found", "").await;
    };
    let Some(relative) = relative(&head.path) else {
        return plain(out, "404 Not Found", "").await;
    };
    // A precompressed sibling serves a client that takes its encoding.
    let mut choice = None;
    for (suffix, encoding) in [(".br", "br"), (".gz", "gzip")] {
        let accepted = head
            .accept_encoding
            .split(',')
            .any(|e| e.split(';').next().is_some_and(|e| e.trim() == encoding));
        if accepted {
            let mut name = relative.clone().into_os_string();
            name.push(suffix);
            if let Some(found) = resolve(&root, Path::new(&name)).await {
                choice = Some((found, Some(encoding)));
                break;
            }
        }
    }
    let choice = match choice {
        Some(choice) => Some(choice),
        None => resolve(&root, &relative).await.map(|f| (f, None)),
    };
    let Some(((file, meta), encoding)) = choice else {
        return plain(out, "404 Not Found", "").await;
    };
    let tag = etag(&meta);
    if head.if_none_match.as_deref() == Some(tag.as_str()) {
        let answer = format!(
            "HTTP/1.1 304 Not Modified\r\nETag: {tag}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n"
        );
        return send(out, answer.as_bytes()).await;
    }
    let mut extra = format!("ETag: {tag}\r\nCache-Control: no-cache\r\nVary: Accept-Encoding\r\n");
    if let Some(encoding) = encoding {
        extra.push_str(&format!("Content-Encoding: {encoding}\r\n"));
    }
    // The type is that of the file asked for, not of its compressed sibling.
    let head_bytes = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
         X-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n{extra}\r\n",
        media_type(&relative),
        meta.len()
    );
    send(out, head_bytes.as_bytes()).await?;
    if !sending_body {
        return Ok(());
    }
    let mut file = tokio::fs::File::open(&file).await?;
    let mut left = meta.len();
    let mut buffer = vec![0u8; CHUNK];
    while left > 0 {
        let want = usize::try_from(left).unwrap_or(CHUNK).min(CHUNK);
        let n = file.read(&mut buffer[..want]).await?;
        if n == 0 {
            break;
        }
        send(out, &buffer[..n]).await?;
        left -= n as u64;
    }
    out.flush().await
}
