use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, ValueEnum};

use crate::error::{Error, Result};
use crate::limits::{LimitConfig, MaxStartups, RateSpec};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Role {
    Listener,
    Monitor,
    Child,
}

#[derive(Debug, Clone, Parser)]
#[command(
    name = "consoled",
    about = "Web-native Linux remote console (xterm.js / WSS / PTY / login+PAM)"
)]
pub struct Args {
    /// Listen address (TLS is required).
    #[arg(long, default_value = "0.0.0.0:8443")]
    pub listen: SocketAddr,

    /// PEM certificate chain.
    #[arg(long)]
    pub cert: PathBuf,

    /// PEM private key.
    #[arg(long)]
    pub key: PathBuf,

    /// Allowed browser Origin, exact match. Repeatable.
    #[arg(long = "origin", required_unless_present = "internal_role")]
    pub origins: Vec<String>,

    /// Unprivileged network-child user.
    #[arg(long, default_value = "consoled")]
    pub user: String,

    /// Empty chroot directory for the network child.
    #[arg(long, default_value = "/var/empty")]
    pub chroot: PathBuf,

    /// Path to login(1).
    #[arg(long, default_value = "/bin/login")]
    pub login: PathBuf,

    /// Maximum WebSocket message size in bytes.
    #[arg(long, default_value_t = 65536)]
    pub max_message_size: usize,

    /// sshd-style MaxStartups start:rate:full.
    #[arg(long, default_value = "50:30:200")]
    pub max_startups: String,

    /// Hard cap on accepted TCP connections (static + console).
    #[arg(long, default_value_t = 500)]
    pub max_tcp: u32,

    /// Cap on concurrent login sessions that have started.
    #[arg(long, default_value_t = 100)]
    pub max_sessions: u32,

    /// Per-source concurrent TCP connections.
    #[arg(long, default_value_t = 20)]
    pub per_source_max: u32,

    /// Per-source new-connection rate as COUNT/SECONDS.
    #[arg(long, default_value = "30/60")]
    pub per_source_rate: String,

    /// Cooldown applied to a source after a very short login session.
    #[arg(long, default_value_t = 60)]
    pub per_source_penalty_seconds: u64,

    /// Session duration below this many seconds counts as "very short".
    #[arg(long, default_value_t = 15)]
    pub per_source_penalty_threshold_seconds: u64,

    /// How long to wait for the initial resize before closing.
    #[arg(long, default_value_t = 10)]
    pub initial_resize_timeout: u64,

    /// Hard cap on connection lifetime (0 disables).
    #[arg(long, default_value_t = 86400)]
    pub max_connection_lifetime: u64,

    /// Idle timeout with no I/O (0 disables).
    #[arg(long, default_value_t = 0)]
    pub idle_timeout: u64,

    /// Seconds to wait after SIGHUP before SIGTERM.
    #[arg(long, default_value_t = 3)]
    pub cleanup_hup_timeout: u64,

    /// Seconds to wait after SIGTERM before SIGKILL.
    #[arg(long, default_value_t = 2)]
    pub cleanup_term_timeout: u64,

    /// Disable seccomp in the network child (debug only).
    #[arg(long, default_value_t = false)]
    pub no_seccomp: bool,

    /// Disable chroot/setuid in the network child (debug only).
    #[arg(long, default_value_t = false)]
    pub no_privdrop: bool,

    #[arg(long, hide = true)]
    pub internal_role: Option<Role>,

    #[arg(long, hide = true)]
    pub client_fd: Option<i32>,

    #[arg(long, hide = true)]
    pub ipc_fd: Option<i32>,

    #[arg(long, hide = true)]
    pub peer: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub listen: SocketAddr,
    pub cert: PathBuf,
    pub key: PathBuf,
    pub origins: Vec<String>,
    pub user: String,
    pub chroot: PathBuf,
    pub login: PathBuf,
    pub max_message_size: usize,
    pub limits: LimitConfig,
    pub initial_resize_timeout: Duration,
    pub max_connection_lifetime: Option<Duration>,
    pub idle_timeout: Option<Duration>,
    pub cleanup_hup_timeout: Duration,
    pub cleanup_term_timeout: Duration,
    pub no_seccomp: bool,
    pub no_privdrop: bool,
}

impl Config {
    pub fn from_args(args: &Args) -> Result<Self> {
        if args.max_message_size < 1024 {
            return Err(Error::msg("--max-message-size must be at least 1024"));
        }
        let max_startups = MaxStartups::parse(&args.max_startups).map_err(Error::msg)?;
        let per_source_rate = RateSpec::parse(&args.per_source_rate).map_err(Error::msg)?;
        let origins: Vec<String> = args
            .origins
            .iter()
            .map(|o| o.trim_end_matches('/').to_string())
            .filter(|o| !o.is_empty())
            .collect();
        if args.internal_role.is_none() && origins.is_empty() {
            return Err(Error::msg("at least one --origin is required"));
        }
        for origin in &origins {
            if !(origin.starts_with("https://") || origin.starts_with("http://")) {
                return Err(Error::msg(format!(
                    "origin must be an absolute URL: {origin}"
                )));
            }
        }
        Ok(Self {
            listen: args.listen,
            cert: args.cert.clone(),
            key: args.key.clone(),
            origins,
            user: args.user.clone(),
            chroot: args.chroot.clone(),
            login: args.login.clone(),
            max_message_size: args.max_message_size,
            limits: LimitConfig {
                max_startups,
                max_tcp: args.max_tcp,
                max_sessions: args.max_sessions,
                per_source_max: args.per_source_max,
                per_source_rate,
                penalty_seconds: args.per_source_penalty_seconds,
                penalty_threshold: Duration::from_secs(args.per_source_penalty_threshold_seconds),
            },
            initial_resize_timeout: Duration::from_secs(args.initial_resize_timeout),
            max_connection_lifetime: nonzero_secs(args.max_connection_lifetime),
            idle_timeout: nonzero_secs(args.idle_timeout),
            cleanup_hup_timeout: Duration::from_secs(args.cleanup_hup_timeout),
            cleanup_term_timeout: Duration::from_secs(args.cleanup_term_timeout),
            no_seccomp: args.no_seccomp,
            no_privdrop: args.no_privdrop,
        })
    }

    pub fn origin_allowed(&self, origin: Option<&str>) -> bool {
        let Some(origin) = origin else {
            return false;
        };
        let origin = origin.trim_end_matches('/');
        self.origins.iter().any(|allowed| allowed == origin)
    }

    pub fn push_reexec_args(&self, cmd: &mut std::process::Command) {
        cmd.arg("--listen").arg(self.listen.to_string());
        cmd.arg("--cert").arg(&self.cert);
        cmd.arg("--key").arg(&self.key);
        for origin in &self.origins {
            cmd.arg("--origin").arg(origin);
        }
        cmd.arg("--user").arg(&self.user);
        cmd.arg("--chroot").arg(&self.chroot);
        cmd.arg("--login").arg(&self.login);
        cmd.arg("--max-message-size")
            .arg(self.max_message_size.to_string());
        cmd.arg("--max-startups").arg(format!(
            "{}:{}:{}",
            self.limits.max_startups.start,
            self.limits.max_startups.rate,
            self.limits.max_startups.full
        ));
        cmd.arg("--max-tcp").arg(self.limits.max_tcp.to_string());
        cmd.arg("--max-sessions")
            .arg(self.limits.max_sessions.to_string());
        cmd.arg("--per-source-max")
            .arg(self.limits.per_source_max.to_string());
        cmd.arg("--per-source-rate").arg(format!(
            "{}/{}",
            self.limits.per_source_rate.count,
            self.limits.per_source_rate.window.as_secs()
        ));
        cmd.arg("--per-source-penalty-seconds")
            .arg(self.limits.penalty_seconds.to_string());
        cmd.arg("--per-source-penalty-threshold-seconds")
            .arg(self.limits.penalty_threshold.as_secs().to_string());
        cmd.arg("--initial-resize-timeout")
            .arg(self.initial_resize_timeout.as_secs().to_string());
        cmd.arg("--max-connection-lifetime").arg(
            self.max_connection_lifetime
                .unwrap_or_default()
                .as_secs()
                .to_string(),
        );
        cmd.arg("--idle-timeout")
            .arg(self.idle_timeout.unwrap_or_default().as_secs().to_string());
        cmd.arg("--cleanup-hup-timeout")
            .arg(self.cleanup_hup_timeout.as_secs().to_string());
        cmd.arg("--cleanup-term-timeout")
            .arg(self.cleanup_term_timeout.as_secs().to_string());
        if self.no_seccomp {
            cmd.arg("--no-seccomp");
        }
        if self.no_privdrop {
            cmd.arg("--no-privdrop");
        }
    }
}

fn nonzero_secs(secs: u64) -> Option<Duration> {
    if secs == 0 {
        None
    } else {
        Some(Duration::from_secs(secs))
    }
}
