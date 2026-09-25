# consoled

Web-native Linux remote console: a browser full-window [xterm.js](https://xtermjs.org/)
terminal over WSS to a PTY running the system's `/bin/login` + PAM.

consoled is a secure remote **PTY transport**. It is not SSH, not a remote-shell
protocol, and it does not authenticate users itself.

```
  browser (xterm.js)
       |  HTTPS GET /     WSS /console
       v
  +------------------+
  | listener (root)  |  accept + limits
  +--------+---------+
           | re-exec
           v
  +------------------+   socketpair    +---------------------------+
  | monitor (root)   |<--------------->| child (user consoled)     |
  | PTY + login      |  SCM_RIGHTS fd  | TLS, HTTP, WS, PTY I/O    |
  | session cleanup  |                 | chroot + nnp + seccomp    |
  +--------+---------+                 +---------------------------+
           | exec
           v
    /bin/login -h <ip>  -->  PAM  -->  user shell
```

## Build

Needs Rust 1.83+ (the pinned crate set is chosen for 1.83).

```bash
cargo build --release
# binary: target/release/consoled
```

```bash
cargo test
cargo clippy --all-targets -- -D warnings
```

Containerized login tests (Docker, privileged). The image compiles
consoled itself in a `rust:1.83-bookworm` stage, so no host build is
needed and the binary matches the Debian 12 runtime's glibc:

```bash
docker build -f integration/Dockerfile -t consoled-it .
docker run --rm --privileged consoled-it
```

## Install the system user and chroot

```bash
sudo useradd --system --no-create-home --shell /usr/sbin/nologin consoled
sudo mkdir -p /var/empty
sudo chown root:root /var/empty
sudo chmod 755 /var/empty
```

## Dev certificate

Self-signed is fine for development:

```bash
openssl req -x509 -newkey rsa:2048 -sha256 -days 365 -nodes \
  -keyout key.pem -out cert.pem \
  -subj "/CN=localhost" \
  -addext "subjectAltName=DNS:localhost,IP:127.0.0.1"
```

Browsers will warn. Accept the warning, or trust the cert in the OS store.

## Run

Must run as root (login + PAM need euid 0).

```bash
sudo ./target/release/consoled \
  --listen 0.0.0.0:8443 \
  --cert ./cert.pem \
  --key ./key.pem \
  --origin https://127.0.0.1:8443
```

Then open `https://127.0.0.1:8443/`. `--origin` must match the URL bar
exactly, including scheme and port. Repeat `--origin` for each allowed
name (`https://host`, `https://host:8443`, ...). Origins are **not**
derived from the Host header.

Listen on 8443 for development. Port 443 needs `CAP_NET_BIND_SERVICE` or
root, same as any other daemon.

## CLI

| Flag | Default | Meaning |
|------|---------|---------|
| `--listen` | `0.0.0.0:8443` | TLS bind address |
| `--cert` / `--key` | required | PEM chain and private key |
| `--origin` | required | allowed Origin (repeatable) |
| `--user` | `consoled` | network-child system user |
| `--chroot` | `/var/empty` | empty jail for the child |
| `--login` | `/bin/login` | login binary |
| `--max-message-size` | `65536` | WebSocket max (bytes) |
| `--max-startups` | `50:30:200` | sshd-style start:rate:full |
| `--max-tcp` | `500` | accepted TCP cap |
| `--max-sessions` | `100` | started login sessions |
| `--per-source-max` | `20` | concurrent sockets per IP |
| `--per-source-rate` | `30/60` | new sockets per IP per seconds |
| `--per-source-penalty-seconds` | `60` | cooldown after a short session |
| `--per-source-penalty-threshold-seconds` | `15` | "short" session length |
| `--initial-resize-timeout` | `10` | wait for first resize |
| `--max-connection-lifetime` | `86400` | hard cap (0 disables) |
| `--idle-timeout` | `0` | idle cap (0 disables) |
| `--no-seccomp` / `--no-privdrop` | off | debug only; reduces isolation |

## Required privileges

- **Listener / monitor / login**: euid 0. login(1) will not run without it.
- **`CAP_NET_BIND_SERVICE`**: only if the listen port is below 1024.
- **`openpty` / `grantpt`**: monitor, as root.
- **Signals + waitpid**: monitor cleans up the login session.
- **Network child**: starts root, loads the TLS key, then chroot + setuid
  to `consoled` + `PR_SET_NO_NEW_PRIVS` + seccomp. After that it only needs
  the inherited client socket, the monitor socket, and (later) the PTY
  master fd.

See `docs/PLAN.md` for the full privilege-separation design.

## Security assumptions

- The TLS certificate is the only server authentication (Web PKI). This is
  **not** mTLS.
- User authentication is entirely `/bin/login` + PAM. consoled never reads
  `/etc/shadow` and never parses passwords.
- termios, echo, ICANON/ISIG, job control, and signals stay in the kernel.
  A browser Ctrl+C is the byte `0x03` written to the PTY master.
- Origin matching is a browser-policy check. **It does not stop scripted
  brute force.** A client can send any Origin header.
- The MVP loads the TLS private key into the network child before dropping
  privileges (see PLAN: weaker than sshd keeping keys in the monitor).
- Defaults are intentionally lenient (browser page loads open several TCP
  sockets). Tighten them before exposing the daemon past an internal net.

### Internet deployments

The design is meant to be internet-ready by configuration, not by extra
features. For a public bind address:

1. Use a public CA certificate (or ACME). Keep `--origin` exact.
2. Tighten `--max-startups`, `--per-source-max`, `--per-source-rate`, and
   penalty knobs toward sshd-like values (`10:30:100`, few sockets per IP).
3. Enable **pam_faillock** (or equivalent) in the PAM `login` stack so
   repeated password failures lock the account. consoled cannot do this.
4. Point fail2ban at journald/stderr. Useful lines:

   `consoled: event=accept src=...`
   `consoled: event=session_end src=... duration_s=... reason=...`
   `consoled: event=drop src=... reason=...`

5. Rely on login.defs `LOGIN_TIMEOUT` plus `--max-connection-lifetime` /
   `--idle-timeout` as the pre-auth backstop.
6. Host firewall, sshd-style `AllowUsers` via PAM `pam_access` if needed.

## Known limitations

- No session resume, reconnect, or multiplexed tabs.
- No file transfer, clipboard protocol, or recording.
- Intentionally detached processes (`tmux`, `nohup`, `setsid`, `disown`)
  are not killed; that is logind `KillUserProcesses`.
- Debian 13 `login` is util-linux and uses PAM service `remote` for
  `login -h`. Ensure `/etc/pam.d/remote` exists on those hosts.
- `pam_loginuid` is `required` on Ubuntu/Debian login PAM and can fail
  inside unprivileged containers.
- Seccomp allows `clone`/`clone3` so Tokio can start threads; `execve` is
  denied.

## Protocol and tests

- Protocol: [`docs/PROTOCOL.md`](docs/PROTOCOL.md)
- Design: [`docs/PLAN.md`](docs/PLAN.md)
- Manual Ubuntu / browser checklist: [`docs/ACCEPTANCE.md`](docs/ACCEPTANCE.md)

xterm.js (`@xterm/xterm` 5.5.0) and `@xterm/addon-fit` 0.10.0 are vendored
under `frontend/vendor/` (MIT).
