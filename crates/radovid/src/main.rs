//! The radovid command line.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use radovid::config::Config;
use radovid::db::Db;
use radovid::service::Service;
use radovid::tls;

#[derive(Parser)]
#[command(about = "Accounts, matchmaking, ratings and game-server launching for any game")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the service.
    Serve {
        #[arg(long, default_value = "radovid.toml")]
        config: PathBuf,
    },
    /// Make an existing account an admin. Run on the host; no message of the protocol does this.
    Admin {
        #[arg(long, default_value = "radovid.toml")]
        config: PathBuf,
        login: String,
    },
    /// Write a self-signed certificate and key for development.
    DevCert {
        /// Directory to write `cert.pem` and `key.pem` into.
        out: PathBuf,
        /// Names the certificate is valid for.
        #[arg(long, default_value = "localhost")]
        name: Vec<String>,
    },
}

fn main() {
    if let Err(error) = run(Args::parse()) {
        eprintln!("radovid: {error}");
        std::process::exit(1);
    }
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())
}

/// Waits for SIGINT or SIGTERM; on SIGHUP rereads the certificate and key
/// (a renewal), keeping the old ones if the new ones do not load.
async fn serve_until_stopped(service: &Service, cert: &Path, key: &Path) -> Result<(), String> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut hangup = signal(SignalKind::hangup()).map_err(|e| e.to_string())?;
    let mut terminate = signal(SignalKind::terminate()).map_err(|e| e.to_string())?;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            _ = terminate.recv() => return Ok(()),
            _ = hangup.recv() => {
                match tls::load(cert, key).and_then(|identity| service.reload_tls(&identity)) {
                    Ok(()) => eprintln!("radovid: certificate reloaded"),
                    Err(error) => eprintln!("radovid: certificate not reloaded: {error}"),
                }
            }
        }
    }
}

fn run(args: Args) -> Result<(), String> {
    match args.command {
        Command::Serve { config } => {
            let config = Config::load(&config)?;
            let identity = tls::load(&config.tls_cert, &config.tls_key)?;
            let (cert, key) = (config.tls_cert.clone(), config.tls_key.clone());
            runtime()?.block_on(async {
                let service = Service::start(config, &identity).await?;
                eprintln!("radovid: listening on {}", service.addr);
                serve_until_stopped(&service, &cert, &key).await?;
                eprintln!("radovid: stopping");
                service.shutdown().await;
                Ok(())
            })
        }
        Command::Admin { config, login } => {
            let config = Config::load(&config)?;
            runtime()?.block_on(async {
                let db = Db::open(&config.database).map_err(|e| e.to_string())?;
                let granted = db
                    .grant_admin(login.to_ascii_lowercase())
                    .await
                    .map_err(|e| e.to_string())?;
                if granted {
                    Ok(())
                } else {
                    Err(format!("no account {login}"))
                }
            })
        }
        Command::DevCert { out, name } => {
            let identity = tls::self_signed(name)?;
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            std::fs::write(out.join("cert.pem"), identity.cert_pem).map_err(|e| e.to_string())?;
            std::fs::write(out.join("key.pem"), identity.key_pem).map_err(|e| e.to_string())?;
            Ok(())
        }
    }
}
