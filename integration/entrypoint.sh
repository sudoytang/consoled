#!/bin/sh
set -eu

useradd --system --no-create-home --shell /usr/sbin/nologin consoled 2>/dev/null || true
mkdir -p /var/empty
chmod 755 /var/empty

if ! id testuser >/dev/null 2>&1; then
    useradd -m -s /bin/bash testuser
fi
echo 'testuser:testpass' | chpasswd

# Containers often cannot set loginuid; keep login(1) usable.
if [ -f /etc/pam.d/login ]; then
    sed -i 's/^session\s\+required\s\+pam_loginuid.so/session optional pam_loginuid.so/' /etc/pam.d/login || true
fi

openssl req -x509 -newkey rsa:2048 \
    -keyout /tmp/key.pem -out /tmp/cert.pem \
    -days 1 -nodes -subj '/CN=127.0.0.1' >/dev/null 2>&1

consoled \
    --listen 127.0.0.1:8443 \
    --cert /tmp/cert.pem \
    --key /tmp/key.pem \
    --origin https://127.0.0.1:8443 \
    --initial-resize-timeout 15 \
    --per-source-penalty-seconds 0 \
    --per-source-max 50 \
    --per-source-rate 100/60 \
    >/tmp/consoled.stdout 2>/tmp/consoled.stderr &
daemon_pid=$!

cleanup() {
    kill -TERM "$daemon_pid" 2>/dev/null || true
    wait "$daemon_pid" 2>/dev/null || true
}
trap cleanup EXIT

i=0
while [ "$i" -lt 50 ]; do
    if python3 - <<'PY' >/dev/null 2>&1
import socket
s = socket.socket()
s.settimeout(0.2)
s.connect(("127.0.0.1", 8443))
s.close()
PY
    then
        break
    fi
    i=$((i + 1))
    sleep 0.1
done

if [ "$i" -eq 50 ]; then
    echo "consoled did not start listening" >&2
    cat /tmp/consoled.stderr >&2 || true
    exit 1
fi

python3 /tests/run_tests.py
echo "integration tests passed"
