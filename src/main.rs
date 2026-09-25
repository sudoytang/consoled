mod child;
mod config;
mod error;
mod http;
mod ipc;
mod limits;
mod linux;
mod listener;
mod monitor;
mod privilege;
mod protocol;
mod pty;
mod session;
mod static_files;
mod websocket;

use clap::Parser;

use crate::config::{Args, Config, Role};
use crate::error::Result;

fn main() {
    if let Err(e) = try_main() {
        eprintln!("consoled: {e}");
        std::process::exit(1);
    }
}

fn try_main() -> Result<()> {
    init_tracing();
    let args = Args::parse();
    let cfg = Config::from_args(&args)?;
    match args.internal_role.unwrap_or(Role::Listener) {
        Role::Listener => listener::run(cfg),
        Role::Monitor => {
            let fd = args
                .client_fd
                .ok_or_else(|| error::Error::msg("monitor missing --client-fd"))?;
            let peer = args.peer.unwrap_or_else(|| "0.0.0.0:0".into());
            monitor::run(cfg, fd, peer)
        }
        Role::Child => {
            let client_fd = args
                .client_fd
                .ok_or_else(|| error::Error::msg("child missing --client-fd"))?;
            let ipc_fd = args
                .ipc_fd
                .ok_or_else(|| error::Error::msg("child missing --ipc-fd"))?;
            let peer = args
                .peer
                .as_deref()
                .unwrap_or("0.0.0.0:0")
                .parse()
                .map_err(|_| error::Error::msg("invalid --peer"))?;
            child::run(cfg, client_fd, ipc_fd, peer)
        }
    }
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_target(false)
        .init();
}
