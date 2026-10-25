//! A directory of files served on the control port's own origin: a page, its types, its
//! compressed siblings, what is refused (paths out of the directory, other methods, long heads)
//! and the control socket still upgrading on the same port.

mod common;

use common::*;
use radovid::config::WebConfig;

fn rules() -> Vec<radovid::config::RulesetConfig> {
    vec![ruleset("solo", true, "team0", "0.3")]
}

/// A site under a folder of its own, a secret beside it, and a link inside it to the secret.
fn site() -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let base = std::env::temp_dir().join(format!(
        "radovid-web-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("site");
    std::fs::create_dir_all(root.join("sub")).expect("folders");
    std::fs::write(base.join("secret.txt"), "the secret").expect("secret");
    std::fs::write(root.join("index.html"), "<h1>page</h1>").expect("index");
    std::fs::write(root.join("app.js"), "console.log(1)").expect("js");
    std::fs::write(root.join("app.js.br"), "BROTLI").expect("br");
    std::fs::write(root.join("big.wasm"), vec![7u8; 300_000]).expect("wasm");
    std::fs::write(root.join("style.css"), "a{}").expect("css");
    std::fs::write(root.join("sub/pic.png"), "PNG").expect("png");
    std::fs::write(root.join("tex.ktx2"), "KTX").expect("ktx2");
    std::fs::write(root.join(".hidden"), "hidden").expect("hidden");
    #[cfg(unix)]
    std::os::unix::fs::symlink(base.join("secret.txt"), root.join("leak")).expect("link");
    root
}

fn served(root: &std::path::Path) -> Env {
    let root = root.to_owned();
    start_configured(rules(), move |config| {
        config.web = Some(WebConfig { static_dir: root });
    })
}

fn get(env: &Env, path: &str, extra: &str) -> (String, Vec<u8>) {
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n{extra}\r\n");
    split(env.https(request.as_bytes()))
}

/// The head as text and the body.
fn split(answer: Vec<u8>) -> (String, Vec<u8>) {
    let at = answer
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map_or(answer.len(), |at| at + 4);
    (
        String::from_utf8_lossy(&answer[..at]).into_owned(),
        answer[at..].to_vec(),
    )
}

fn status(head: &str) -> &str {
    head.lines().next().unwrap_or("")
}

#[test]
fn a_page_loads_with_its_types_and_the_control_socket_still_upgrades() {
    let root = site();
    let env = served(&root);
    let (head, body) = get(&env, "/", "");
    assert_eq!(status(&head), "HTTP/1.1 200 OK", "{head}");
    assert!(
        head.contains("Content-Type: text/html; charset=utf-8"),
        "{head}"
    );
    assert!(head.contains("X-Content-Type-Options: nosniff"), "{head}");
    assert_eq!(body, b"<h1>page</h1>");
    for (path, kind) in [
        ("/app.js", "text/javascript; charset=utf-8"),
        ("/big.wasm", "application/wasm"),
        ("/style.css", "text/css; charset=utf-8"),
        ("/sub/pic.png", "image/png"),
        ("/tex.ktx2", "image/ktx2"),
    ] {
        let (head, _) = get(&env, path, "");
        assert_eq!(status(&head), "HTTP/1.1 200 OK", "{path}: {head}");
        assert!(
            head.contains(&format!("Content-Type: {kind}")),
            "{path}: {head}"
        );
    }
    // A large file arrives whole; HEAD sends the head alone.
    let (head, body) = get(&env, "/big.wasm", "");
    assert_eq!(body.len(), 300_000, "{head}");
    let (head, body) = split(env.https(b"HEAD /big.wasm HTTP/1.1\r\nHost: localhost\r\n\r\n"));
    assert!(
        head.contains("Content-Length: 300000") && body.is_empty(),
        "{head}"
    );

    // The compressed sibling goes to a client that takes it, under the file's own type.
    let (head, body) = get(&env, "/app.js", "Accept-Encoding: gzip, br\r\n");
    assert!(head.contains("Content-Encoding: br"), "{head}");
    assert!(head.contains("Content-Type: text/javascript"), "{head}");
    assert_eq!(body, b"BROTLI");
    // The tag lets a client keep what it has.
    let tag = get(&env, "/style.css", "").0;
    let tag = tag
        .lines()
        .find_map(|l| l.strip_prefix("ETag: "))
        .expect("a tag")
        .to_owned();
    let (head, body) = get(&env, "/style.css", &format!("If-None-Match: {tag}\r\n"));
    assert_eq!(status(&head), "HTTP/1.1 304 Not Modified", "{head}");
    assert!(body.is_empty());

    // The control socket upgrades on the same port.
    let mut account = env.human("webby");
    assert!(
        account
            .ask(radovid_client::proto::ClientMsg::MyMatches)
            .is_ok()
    );
    let _ = std::fs::remove_dir_all(root.parent().expect("base"));
}

#[test]
fn what_leaves_the_directory_or_is_not_a_plain_file_is_refused() {
    let root = site();
    let env = served(&root);
    let paths = [
        "/../secret.txt",
        "/sub/../../secret.txt",
        "/%2e%2e/secret.txt",
        "/sub/%2e%2e/%2e%2e/secret.txt",
        "/..%2fsecret.txt",
        "/sub\\..\\..\\secret.txt",
        "/.hidden",
        "//etc/passwd",
        "/sub/",
        "/sub",
        "/leak",
        "/nothing.js",
        "/a\0b",
    ];
    for path in paths {
        let (head, body) = get(&env, path, "");
        assert!(
            !status(&head).contains("200") && !String::from_utf8_lossy(&body).contains("secret"),
            "{path:?} was served: {head}"
        );
    }
    let (head, _) = split(env.https(b"POST /app.js HTTP/1.1\r\nHost: localhost\r\n\r\n"));
    assert_eq!(status(&head), "HTTP/1.1 405 Method Not Allowed", "{head}");
    assert!(head.contains("Allow: GET, HEAD"), "{head}");
    // A head that never ends is cut off, answered with nothing.
    let long = format!("GET / HTTP/1.1\r\nX: {}\r\n", "a".repeat(20_000));
    assert!(env.https(long.as_bytes()).is_empty());
    // And without a directory the port serves no page.
    let bare = start(rules());
    let (head, _) = split(bare.https(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n"));
    assert!(!status(&head).contains("200"), "{head}");
    let _ = std::fs::remove_dir_all(root.parent().expect("base"));
}
