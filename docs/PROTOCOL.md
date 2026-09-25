# consoled protocol

consoled is a secure remote PTY transport. It is not SSH and does not interpret
terminal bytes.

The browser opens `wss://<host>[:port]/console` on the same origin as the HTTPS
page. TLS is required. The WebSocket is same-origin; Origin is checked against
explicit `--origin` values (never derived from the Host header).

## Framing

- **Binary WebSocket messages** are raw PTY bytes in both directions.
  - Client binary payload is written to the PTY master as-is.
  - Bytes read from the PTY master are sent as binary messages.
  - There is no extra application framing, length prefix, or encoding step.
- **Text WebSocket messages** are JSON control. The only accepted message is:

```json
{"type":"resize","cols":N,"rows":M}
```

`cols` and `rows` are integers in `1..=1000`. The server applies `TIOCSWINSZ` on
the PTY master. No other ioctl is forwarded.

## Initial resize

The client MUST send a resize immediately after the socket opens. The server
starts `/bin/login` only after a valid initial resize. If none arrives before
`--initial-resize-timeout` (default 10s), the server closes the connection.

## Limits

Maximum WebSocket message size is 64 KiB by default (`--max-message-size`).
Oversized messages close the connection.

## Close codes

| Code | Meaning |
|------|---------|
| 1000 | Normal close (session ended / client closed) |
| 1002 | Malformed control JSON, or binary data before the initial resize |
| 1008 | Unknown control type, or resize values out of range |
| 1009 | Message too large (implementation-dependent) |
| 4000 | Initial resize timeout |
| 4001 | Max connection lifetime exceeded |
| 4002 | Idle timeout |

Malformed or unknown control messages close the connection. The server does not
attempt recovery.

## What the server does not do

The server does not parse UTF-8, ANSI, passwords, or job-control characters. A
client `0x03` byte is written to the PTY; the kernel line discipline may turn it
into `SIGINT`. consoled never calls `kill()` in response to input.
