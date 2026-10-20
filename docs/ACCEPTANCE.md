# Manual acceptance checklist

Run these on Ubuntu 24.04 (or Debian 12) against a locally built `consoled`
with a self-signed certificate. Use the origin that matches the URL bar,
including port.

```bash
sudo ./target/release/consoled \
  --listen 127.0.0.1:8443 \
  --cert ./cert.pem \
  --key ./key.pem \
  --origin https://127.0.0.1:8443
```

Open `https://127.0.0.1:8443/` and accept the certificate warning.

## Running in Docker (any host, including macOS)

If you are not on a Debian/Ubuntu machine, run the checklist inside the
integration image. On Apple Silicon this builds a native aarch64 image.

```bash
docker build -f integration/Dockerfile -t consoled-test .
docker run --rm -it --privileged -p 127.0.0.1:8443:8443 \
  --entrypoint bash consoled-test
```

`--privileged` is needed because consoled itself chroots, drops privileges and
installs seccomp, which Docker's default profile does not allow. `--entrypoint
bash` skips the automated test script and gives you a root shell.

Inside the container, do the setup that `integration/entrypoint.sh` does and
start consoled in the foreground:

```bash
useradd --system --no-create-home --shell /usr/sbin/nologin consoled
mkdir -p /var/empty && chmod 755 /var/empty
useradd -m -s /bin/bash testuser && echo 'testuser:testpass' | chpasswd
# Containers usually cannot set loginuid; make it optional for login(1).
sed -i 's/^session\s\+required\s\+pam_loginuid.so/session optional pam_loginuid.so/' /etc/pam.d/login
openssl req -x509 -newkey rsa:2048 -nodes -days 7 \
  -keyout /tmp/key.pem -out /tmp/cert.pem -subj '/CN=localhost'

consoled \
  --listen 0.0.0.0:8443 \
  --cert /tmp/cert.pem --key /tmp/key.pem \
  --origin https://localhost:8443 \
  --origin https://127.0.0.1:8443 \
  --per-source-penalty-seconds 0
```

Then on the host open `https://localhost:8443/` (note `https://`), accept the
certificate warning and log in as `testuser` / `testpass`.

- Listen on `0.0.0.0` inside the container: Docker's port forwarding arrives on
  the container's network interface, not its loopback.
- The origin check is an exact string match, so list every URL you will type
  (`localhost` and `127.0.0.1` are different origins). A mismatch shows a black
  page and `origin_rejected` in consoled's log.
- `--per-source-penalty-seconds 0` disables the short-session cooldown, which
  otherwise kicks in after quick logout/refresh cycles from the same IP.
- To inspect processes during the checks, open a second shell with
  `docker exec -it <container> bash`.

## Browsers

Repeat the functional checks in:

- [ ] Chrome / Chromium
- [ ] Firefox
- [ ] Safari (macOS / iOS), if a machine is available

Safari cannot be exercised in this Linux CI environment.

## Login

- [ ] Full-window terminal; prompt looks like `<hostname> login:`
- [ ] Valid local account + password yields the user shell
- [ ] Wrong password prints `Login incorrect` (or equivalent) and does not
      start a shell
- [ ] After login, `echo $USER` matches the account

## TTY

In the shell:

```bash
tty
python3 -c 'import os; print(os.isatty(0), os.isatty(1), os.isatty(2))'
stty size
```

- [ ] `tty` prints `/dev/pts/N`
- [ ] isatty is `True True True`
- [ ] `stty size` matches the browser window (rows/cols)

Resize the browser window.

- [ ] `stty size` updates
- [ ] Full-screen programs redraw

## Programs

- [ ] `vim` (or `vi`): insert text, arrows, backspace, `:q!`
- [ ] `top` (or `htop`): updates, `q` quits
- [ ] `less /etc/passwd`: arrows, page up/down, `q`
- [ ] UTF-8: `printf 'cafe\u0301 日本語 \u2603\n'`
- [ ] Colors: `ls --color=auto` or `printf '\033[31mred\033[0m\n'`
- [ ] Backspace deletes the previous character at the shell prompt

## Signals (kernel line discipline, not consoled)

```bash
sleep 1000
```

- [ ] Ctrl+C returns to the prompt; `sleep` is gone (`echo $?` is 130)

```bash
sleep 1000
```

- [ ] Ctrl+Z stops the job; `jobs` shows it; `fg` resumes; Ctrl+C finishes it

## Disconnect cleanup

- [ ] Close the tab: no leftover `login` or user-shell process for that session
      (`ps -u "$USER" -o pid,tty,cmd` / `pgrep -a login`)
- [ ] Refresh the tab: old session is gone; a new login prompt appears
- [ ] `exit` in the shell closes the WebSocket and shows `[disconnected]`
- [ ] `sudo kill -TERM <consoled-listener-pid>` ends remaining sessions without
      leaving zombies (`ps --ppid 1 -o pid,cmd` should not show orphaned logins
      from this test)

Detached tools (`tmux`, `nohup`, `disown`, `setsid`) are expected to survive.
That is logind `KillUserProcesses` territory, not consoled.

## Security smoke

- [ ] `curl -k https://127.0.0.1:8443/console` without a WebSocket upgrade is
      rejected
- [ ] A WebSocket client that sends `Origin: https://evil.example` is rejected
      (HTTP 403)
- [ ] Page source / network panel: xterm.js is served from the same origin, not
      a CDN
- [ ] Response headers include `Content-Security-Policy` and
      `Strict-Transport-Security`
- [ ] Journal/stderr shows source IP, duration, and an exit reason, never typed
      passwords or terminal contents
