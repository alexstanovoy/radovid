//! A game server as a child process of the service.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use super::{Backend, BoxFuture, Container, Exit, MatchSpec};

/// How long a game server has to report the port it listens on.
const STARTUP: Duration = Duration::from_secs(20);

pub struct LocalBackend {
    binary: PathBuf,
}

impl LocalBackend {
    pub fn new(binary: PathBuf) -> LocalBackend {
        LocalBackend { binary }
    }

    async fn start_process(&self, spec: &MatchSpec) -> std::io::Result<Container> {
        let mut child = Command::new(&self.binary)
            .args(spec.args(0))
            .env("BOTA_SEATS", spec.seats_env())
            .env("BOTA_REFEREE_KEY", spec.referee_env())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("no stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| std::io::Error::other("no stderr"))?;
        let id = spec.match_id.0;
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                eprintln!("radovid: match {id}: {line}");
            }
        });
        let mut lines = BufReader::new(stdout).lines();
        let port = tokio::time::timeout(STARTUP, async {
            while let Some(line) = lines.next_line().await? {
                if let Some(port) = line
                    .rsplit(':')
                    .next()
                    .filter(|_| line.contains("listening on"))
                {
                    return port.trim().parse::<u16>().map_err(std::io::Error::other);
                }
            }
            Err(std::io::Error::other(
                "the game server exited before listening",
            ))
        })
        .await
        .map_err(|_| std::io::Error::other("the game server never reported its port"))??;
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let (container, exit_tx, mut kill_rx) = Container::new(addr);
        tokio::spawn(async move {
            let status = tokio::select! {
                status = child.wait() => status,
                _ = kill_rx.recv() => {
                    let _ = child.kill().await;
                    child.wait().await
                }
            };
            let description = match status {
                Ok(status) => status.to_string(),
                Err(error) => error.to_string(),
            };
            let _ = exit_tx.send(Some(Exit { description }));
        });
        Ok(container)
    }
}

impl Backend for LocalBackend {
    fn start<'a>(&'a self, spec: &'a MatchSpec) -> BoxFuture<'a, std::io::Result<Container>> {
        Box::pin(self.start_process(spec))
    }

    fn clean_up_orphans(&self) -> BoxFuture<'_, ()> {
        // Children die with the service (`kill_on_drop`, and their sockets close).
        Box::pin(async {})
    }

    fn wire_fingerprint(&self) -> BoxFuture<'_, std::io::Result<u64>> {
        Box::pin(async {
            let output = Command::new(&self.binary)
                .arg("--wire-fingerprint")
                .stdin(Stdio::null())
                .output()
                .await?;
            if !output.status.success() {
                return Err(std::io::Error::other(format!(
                    "{} --wire-fingerprint: {}",
                    self.binary.display(),
                    output.status
                )));
            }
            super::parse_fingerprint(&String::from_utf8_lossy(&output.stdout))
        })
    }
}
