# consoled design and implementation plan

## Goal

consoled is a minimal web-native Linux remote console. A user opens
`https://<server>/` in a modern browser, sees a full-window xterm.js terminal,
and that terminal is attached over WSS (same origin, path `/console`) to a PTY
on which the system's `/bin/login` is running. Authentication is Linux
login + PAM. The browser authenticates the server via the TLS certificate (Web
PKI). This is not mTLS, not SSH, and not a remote-shell protocol. consoled is
only a secure remote PTY transport.

Hard rules:

- consoled MUST NOT verify usernames/passwords itself
- MUST NOT read `/etc/shadow`
- MUST NOT log or parse terminal input/output
- MUST NOT interpret Ctrl+C/D/Z, ANSI, UTF-8, or password input
- MUST NOT translate Ctrl+C into `kill()`
- termios, line discipline, echo, ICANON/ISIG, job control, signals, and the
  controlling terminal stay with the Linux kernel
- No nginx / reverse proxy in the MVP
- No plaintext production mode (TLS required; self-signed is fine for dev)

## Scope

In:

- HTTPS + WSS
- Static frontend with vendored xterm.js
- PTY allocation and `/bin/login`
- Raw terminal I/O
- Resize (`TIOCSWINSZ`)
- Connection cleanup
- Strict Origin validation
- Connection / rate / message-size limits

Out (do not implement):

- SSH / SFTP / SCP, port forwarding, public keys, WebAuthn
- Custom login UI, session resume / reconnect, file transfer
- Multiple tabs or multiple PTYs per socket
- Clipboard protocol, collaboration, recording, content audit logs
- Custom auth / user DB, admin dashboard
- Docker / Kubernetes integration, Windows / macOS

## Architecture

```
  browser (xterm.js)
           |  HTTPS  GET /
           |  WSS    /console   (binary = PTY bytes, text = resize JSON)
           v
  +---------------------------+
  | listener (root)           |  bind, accept, limits, re-exec monitor
  +-------------+-------------+
                | fork+exec /proc/self/exe --internal-role=monitor
                v
  +---------------------------+     socketpair      +------------------------------+
  | monitor (root, 1 thread)  |<------------------>| network child (user consoled)|
  |  - wait StartSession      |   SCM_RIGHTS fd    |  - load TLS key, then drop   |
  |  - openpty + exec login   |                    |  - chroot /var/empty         |
  |  - wait / cleanup         |                    |  - no_new_privs + seccomp    |
  +-------------+-------------+                    |  - rustls + HTTP + WS        |
                | exec                             |  - PTY I/O + TIOCSWINSZ      |
                v                                  +------------------------------+
         /bin/login -h <ip>
                |
                v
         PAM service "login" (shadow) or "remote" (util-linux -h)
                |
                v
         user shell on the PTY slave
```

Process roles share one binary. The listener never runs a multithreaded Tokio
runtime and then `fork()`s into non-async-signal-safe code. Each hop is
`fork+exec` of `/proc/self/exe` with `--internal-role`. The monitor stays
single-threaded and is the only process that `fork()`s to exec `login`.

## Privilege separation

Modeled on OpenSSH:

1. **Root listener** accepts TCP connections and enforces global / per-IP
   limits (the only place that sees every SYN-accepted socket).
2. **Root per-connection monitor** is re-exec'd with the accepted fd. It
   creates a socketpair, re-execs the network child, and waits for one
   request: *start a login session with this window size and client IP*.
3. **Unprivileged network child** loads the TLS private key, then:
   - `chroot` to an empty directory (`/var/empty` by default)
   - `setgroups` / `setgid` / `setuid` to the `consoled` system user
   - `PR_SET_NO_NEW_PRIVS`
   - a seccomp-bpf allowlist (default-deny, `KillProcess`)
   - starts Tokio and does all TLS / HTTP / WebSocket / JSON parsing

The child's only privileged request is the fixed 128-byte `StartSession`
message. The monitor allocates the PTY, starts `login -h <client IP>` as the
session leader with the slave as controlling tty and stdio, and passes the
master fd back with `SCM_RIGHTS`. The child then does the data loop and
`TIOCSWINSZ` itself. The monitor waits and, on child exit or shutdown,
cleans up the session.

### TLS key placement (MVP tradeoff)

The MVP loads the TLS private key into the network child **before** dropping
privileges. After chroot + setuid the file is no longer readable, but the
key material remains in the child's address space.

sshd keeps host keys in the privileged monitor and only hands out a
short-lived context. That is better isolation against a memory-disclosure
bug in rustls/HTTP/JSON. Doing the same here would require a second IPC
round-trip or a TLS offload in the monitor, which expands the privileged
parser surface. The MVP accepts the weaker placement and documents it.

A later change can move `ServerConfig` construction (or a raw TCP-pass +
key fd) into the monitor without changing the browser protocol.

### Required privileges / capabilities

| Process | Needs | Why |
|---------|-------|-----|
| listener | euid 0 (or equivalent) | re-exec monitors; optional `CAP_NET_BIND_SERVICE` only if listen port < 1024 |
| monitor | euid 0 | `openpty`/`grantpt`, exec `/bin/login`, signal the session, `waitpid` |
| login | euid 0 | PAM (`pam_unix` reads shadow), `setuid` to the user, utmp/wtmp, tty ownership |
| network child | starts as root, then uid `consoled` | TLS/HTTP/WS only after drop; no extra capabilities |

The `consoled` user should be a system account (`nologin`, no home). The
chroot directory must exist, be a directory, and must not be group/world
writable.

Seccomp notes: `clone`/`clone3` are allowed so Tokio can start worker
threads after the filter is installed. `execve` is denied. Restricting
`clone` to `CLONE_THREAD` only is future work; `no_new_privs` + empty
chroot + no `execve` is the MVP fence.

## Process lifecycle

1. Browser connects; listener admits or drops (limits).
2. Monitor + child start; child completes TLS and serves HTTPS.
3. Static GETs (`/`, JS, CSS) never start a PTY. Child exits; monitor
   exits; **no PerSourcePenalty** (no session was started).
4. `GET /console` with a matching Origin upgrades to WebSocket.
5. Child waits for the first resize (timeout -> close).
6. Child sends `StartSession`; monitor execs login; master fd returns.
7. Binary bytes flow both ways. Resize text messages call `TIOCSWINSZ`.
8. On WS close, PTY EOF, timeout, or protocol error, the child exits.
9. Monitor cleanup (sshd-style): SIGHUP every process with that session
   id, wait, SIGTERM, wait, SIGKILL, `waitpid` everything. Processes that
   called `setsid` / `nohup` / `tmux` have a new SID and are left alone.
10. Listener SIGTERM/SIGINT: stop accepting, SIGTERM all monitors, wait,
    SIGKILL leftovers.

When login/shell exits, read on the master returns 0 and the child closes
the WebSocket.

Shadow `login` (PAM) forks: the parent stays root to call
`pam_close_session`, the child becomes the user shell. Cleanup therefore
walks `/proc/*/stat` for the original session id rather than a single
process group.

## Protocol

See `docs/PROTOCOL.md`. Summary: binary = raw PTY; text = resize JSON
only; initial resize required; max message 64 KiB.

## Security model

- Browser -> server: Web PKI (TLS server cert). Not mTLS.
- Server -> user: `/bin/login` + PAM. consoled never sees a password as
  structured data; it only copies bytes.
- Origin: exact match against `--origin` (repeatable). Missing Origin is
  rejected. CORS is not used and not relied on.
- `/console` is only reachable after TLS. There is no HTTP plaintext
  listener.
- Static assets are embedded and served with a strict CSP and other
  hardening headers (nosniff, frame deny, no-referrer, HSTS).
- xterm.js is vendored; no CDN.
- Logs (stderr / journald) record source IP, start, duration, and an exit
  reason. They never record terminal contents.

**Origin checks do not stop scripted brute force.** A non-browser client
can send any Origin it likes, or be given a matching one. Internet
deployments must use PAM (`pam_faillock`), `LOGIN_TIMEOUT` / login.defs,
fail2ban on consoled's session logs, and host firewalling.

## Rate limiting

Modeled on sshd, all configurable, **lenient defaults** because each
browser page load opens several TCP connections (HTML/JS/CSS + WSS):

| Knob | Default | sshd analogue |
|------|---------|----------------|
| `--max-startups start:rate:full` | `50:30:200` | `MaxStartups` (sshd 10:30:100) |
| `--max-tcp` | 500 | hard cap on accepted sockets |
| `--max-sessions` | 100 | started login sessions |
| `--per-source-max` | 20 | `MaxStartups` / `PerSourceMaxStartups` |
| `--per-source-rate` | `30/60` | new sockets per source per window |
| `--per-source-penalty-seconds` | 60 | `PerSourcePenalties` |
| `--per-source-penalty-threshold-seconds` | 15 | short-session signal |

MaxStartups probability: 0 below `start`; `rate/100` at `start`; linear
to 1.0 at `full`.

consoled cannot see PAM success/failure, so a **short login session**
(PTY was started, duration below the threshold) is the penalty signal.
Pure HTTP asset connections do not start a session and do not penalize.

Pre-auth timeout is login's own `LOGIN_TIMEOUT` (`/etc/login.defs`, 60s
on the Ubuntu 24.04 reference host) plus consoled `--max-connection-lifetime`
and optional `--idle-timeout`.

## Distro findings (Debian / Ubuntu)

Verified on Ubuntu 24.04.4 in this workspace and against upstream sources.

### `/bin/login` implementation

| Distro | Package | Source | PAM service |
|--------|---------|--------|-------------|
| Ubuntu 24.04 | `login` 1:4.13+dfsg1-4ubuntu3.2 | **shadow** 4.13 | always `"login"` (`/etc/pam.d/login`) |
| Debian 12 | `login` from shadow | **shadow** | always `"login"` |
| Debian 13 | `login` moved to util-linux | **util-linux** | `"login"`, or **`"remote"`** when `-h` is used |

Ubuntu 24.04 `login` usage (from the binary): `login [-p] [-h host] [-f name]`.
`-h` requires euid 0. It sets `PAM_RHOST`, `REMOTEHOST`, and the utmp host.
It does **not** switch PAM service. `pam_start("login", ...)` is hard-coded
in shadow 4.13 `src/login.c`.

util-linux login (Debian 13+) does:

```c
pam_start(cxt->remote ? "remote" : "login", ...)
```

So `login -h <ip>` on Debian 13 uses `/etc/pam.d/remote`. If that file is
missing, login fails closed. Operators on Debian 13 must ensure `remote`
exists (the util-linux package is expected to ship it, or copy `login`).

### Invocation we use

```
login -h <client-ip>
```

as session leader, slave = controlling tty = stdin/stdout/stderr.
`TERM=xterm-256color` is set in the environment; shadow login preserves
`TERM` even without `-p`. We do **not** pass `-f` (pre-authenticated).
We do **not** pass a username; login prompts.

A standard getty is **not** required. login only needs a real tty on
0/1/2 and euid 0. If not run as root, shadow login refuses (`Cannot
possibly work without effective root`). The historic "No utmp entry. You
must exec login from the lowest level sh" check applies only to non-root
invocations.

### PAM stack (Ubuntu 24.04 `/etc/pam.d/login`)

- `pam_faildelay` (3s) on failed auth
- `pam_nologin`
- `pam_loginuid` (**required** — often fails in unprivileged containers)
- `pam_motd`, `pam_env`, `common-auth` (`pam_unix`)
- `pam_limits`, `pam_lastlog`, `pam_mail`, `pam_keyinit`
- `common-session` includes **optional** `pam_systemd.so` (logind)
- **no `pam_securetty`** in the default Ubuntu 24.04 login stack
- `/etc/securetty` is absent on the reference host

`pam_securetty` on other Debian versions only restricts **root** on
non-listed ttys. `pts/N` is typically not listed, so root-over-console
may be denied where that module is enabled. That is expected and
desirable.

`pam_systemd` / logind: session registration happens if systemd is on
the machine and the module succeeds. It is optional; failure is ignored.
utmp/wtmp updates are login's job and may be best-effort in containers.

`LOGIN_TIMEOUT` in `/etc/login.defs` is 60 seconds on Ubuntu 24.04.

Integration tests that run inside Docker must make `pam_loginuid`
optional (or run `--privileged` with a working audit loginuid). The
provided Debian 12 image does both.

## Testing strategy

1. **Unit tests** (`cargo test`): protocol parse/validate; MaxStartups
   math; per-IP cap/rate; short-session penalty; IPC request codec;
   `/proc` sid parser.
2. **Container integration** (`integration/`): real `/bin/login` in
   Debian 12, test account, WSS client (stdlib Python):
   - login success
   - bad password rejected
   - `tty` is `/dev/pts/N`
   - isatty True True True
   - `stty size` follows resize
   - Ctrl+C kills `sleep 1000`
   - Ctrl+Z job control
   - no leftover login/shell after disconnect
   - bad Origin rejected
3. **CI**: GitHub Actions runs fmt, clippy `-D warnings`, `cargo test`,
   `cargo build --release`, then the privileged Debian container.
4. **Manual**: `docs/ACCEPTANCE.md` for Ubuntu + Chrome/Firefox/Safari.

Do not expose anything beyond localhost in automated tests until TLS,
Origin, limits, and the login path are in place. The integration daemon
binds `127.0.0.1:8443` with TLS and Origin checks.

## Implementation order (what was built, in this sequence)

1. Design (`docs/PLAN.md`) and crate skeleton (modules: `main`, `http`,
   `websocket`, `pty`, `session`, `protocol`, `limits`, plus `listener`,
   `monitor`, `child`, `ipc`, `privilege`, `linux`).
2. Static page + vendored xterm.js / fit addon, served from the binary.
3. WebSocket upgrade on `/console` (same process as HTTPS).
4. PTY abstraction (`spawn_login` / read / write / resize / close / wait)
   with no WebSocket types.
5. Privilege-separated process model (re-exec, socketpair, SCM_RIGHTS).
6. Switch the session program from a temporary shell to `/bin/login -h`.
7. TLS (rustls) required; Origin exact-match; limits; sshd-style cleanup.
8. Unit tests, Debian integration script, CI, acceptance checklist.

## Open questions (not built)

See the pull request description. Notable ones: whether to move the TLS
key into the monitor; whether Debian 13 should ship a sample
`/etc/pam.d/remote`; whether a systemd unit belongs in a later change.
