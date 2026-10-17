//! A game server as a docker container on a private network: read-only root,
//! no volume, no published port, secrets by environment.

use std::net::{IpAddr, SocketAddr};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use super::{Backend, BoxFuture, Container, Exit, MatchSpec};

/// The port the game server listens on inside the container.
const CONTAINER_PORT: u16 = 4455;
/// The label every container carries, for cleaning up after a crashed service.
const LABEL: &str = "radovid.match";

pub struct DockerBackend {
    docker: String,
    image: String,
    network: String,
    memory: String,
    cpus: String,
    publish_loopback: bool,
}

impl DockerBackend {
    pub fn new(
        docker: String,
        image: String,
        network: String,
        memory: String,
        cpus: String,
        publish_loopback: bool,
    ) -> Self {
        DockerBackend {
            publish_loopback,
            docker,
            image,
            network,
            memory,
            cpus,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.docker);
        command.stdin(Stdio::null()).kill_on_drop(false);
        command
    }

    async fn output(&self, args: &[&str]) -> std::io::Result<String> {
        let output = self.command().args(args).output().await?;
        if !output.status.success() {
            return Err(std::io::Error::other(format!(
                "docker {}: {}",
                args.first().copied().unwrap_or(""),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    /// Where the container's game port can be reached from here.
    async fn address_of(&self, id: &str) -> std::io::Result<SocketAddr> {
        if self.publish_loopback {
            let port = self
                .output(&["port", id, &format!("{CONTAINER_PORT}/tcp")])
                .await?;
            // "127.0.0.1:49153", one line per address family.
            let line = port.lines().next().unwrap_or("");
            return line
                .parse()
                .map_err(|_| std::io::Error::other(format!("docker port: {line:?}")));
        }
        let format = format!(
            "{{{{(index .NetworkSettings.Networks \"{}\").IPAddress}}}}",
            self.network
        );
        let ip: IpAddr = self
            .output(&["inspect", "-f", &format, id])
            .await?
            .parse()
            .map_err(std::io::Error::other)?;
        Ok(SocketAddr::new(ip, CONTAINER_PORT))
    }

    async fn start_container(&self, spec: &MatchSpec) -> std::io::Result<Container> {
        let label = format!("{LABEL}={}", spec.match_id.0);
        let name = format!("radovid-match-{}", spec.match_id.0);
        let mut args: Vec<String> = [
            "run",
            "--detach",
            "--name",
            &name,
            "--read-only",
            "--network",
            &self.network,
            "--memory",
            &self.memory,
            "--cpus",
            &self.cpus,
            "--pids-limit",
            "256",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--label",
            &label,
            "-e",
            "BOTA_SEATS",
            "-e",
            "BOTA_REFEREE_KEY",
        ]
        .map(str::to_owned)
        .to_vec();
        if self.publish_loopback {
            args.extend(["-p".to_owned(), format!("127.0.0.1::{CONTAINER_PORT}")]);
        }
        args.push(self.image.clone());
        args.extend(spec.args(CONTAINER_PORT));
        let output = self
            .command()
            .args(&args)
            .env("BOTA_SEATS", spec.seats_env())
            .env("BOTA_REFEREE_KEY", spec.referee_env())
            .output()
            .await?;
        if !output.status.success() {
            return Err(std::io::Error::other(format!(
                "docker run: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let addr = match self.address_of(&id).await {
            Ok(addr) => addr,
            Err(error) => {
                let _ = self.output(&["rm", "-f", &id]).await;
                return Err(error);
            }
        };
        let (container, exit_tx, mut kill_rx) = Container::new(addr);
        let docker = self.docker.clone();
        let match_id = spec.match_id.0;
        tokio::spawn(async move {
            let wait = Command::new(&docker)
                .args(["wait", &id])
                .stdin(Stdio::null())
                .output();
            tokio::pin!(wait);
            let mut killed = false;
            let code = loop {
                tokio::select! {
                    output = &mut wait => break output.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned()),
                    Some(()) = kill_rx.recv(), if !killed => {
                        killed = true;
                        let _ = Command::new(&docker).args(["kill", &id]).stdin(Stdio::null()).output().await;
                    }
                }
            };
            if let Ok(logs) = Command::new(&docker)
                .args(["logs", "--tail", "20", &id])
                .output()
                .await
            {
                for line in String::from_utf8_lossy(&logs.stderr).lines() {
                    eprintln!("radovid: match {match_id}: {line}");
                }
            }
            let _ = Command::new(&docker)
                .args(["rm", "-f", &id])
                .stdin(Stdio::null())
                .output()
                .await;
            let description = match code {
                Ok(code) => format!("exit code {code}"),
                Err(error) => error.to_string(),
            };
            let _ = exit_tx.send(Some(Exit { description }));
        });
        Ok(container)
    }
}

impl Backend for DockerBackend {
    fn start<'a>(&'a self, spec: &'a MatchSpec) -> BoxFuture<'a, std::io::Result<Container>> {
        Box::pin(self.start_container(spec))
    }

    fn clean_up_orphans(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let filter = format!("label={LABEL}");
            let listed = tokio::time::timeout(
                Duration::from_secs(20),
                self.output(&["ps", "-aq", "--filter", &filter]),
            )
            .await;
            let Ok(Ok(ids)) = listed else {
                return;
            };
            for id in ids.split_whitespace() {
                let _ = self.output(&["rm", "-f", id]).await;
            }
        })
    }

    fn wire_fingerprint(&self) -> BoxFuture<'_, std::io::Result<u64>> {
        Box::pin(async {
            let printed = self
                .output(&[
                    "run",
                    "--rm",
                    "--network",
                    "none",
                    "--read-only",
                    "--cap-drop",
                    "ALL",
                    "--security-opt",
                    "no-new-privileges",
                    &self.image,
                    "--wire-fingerprint",
                ])
                .await?;
            super::parse_fingerprint(&printed)
        })
    }
}
