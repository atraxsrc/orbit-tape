# Security

## Reporting a vulnerability

Please report security problems privately through GitHub's
[private vulnerability reporting](https://github.com/atraxsrc/orbit-tape/security/advisories/new)
for this repository, not in a public issue. Include the version
(Settings shows it), what you did and what happened. You should get a reply
within a week.

Only the latest release gets security fixes.

## TLS certificates

Orbit Tape verifies TLS certificates for every HTTPS and WSS connection:
exchanges, the indicator catalog and the optional market-data server.

The one exception is an explicit opt-in for a self-hosted market-data server.
Under Settings > Network > Historical backfill > Server, **Accept invalid TLS
certificates** turns certificate checks off for that server only (exchanges
and the catalog are never affected). The app shows a warning while it is on
and writes a warning to the log at startup. With checks off, anyone on the
network path can read or change the server's data and see its auth token, so
use it only for a server with a self-signed certificate on a network you
trust. It is off for new installs and for configs saved by older versions.

## Credentials

Orbit Tape needs no exchange API keys and places no orders. Proxy passwords and
the market-data server token are stored in the system keyring, never in the
JSON settings files.

## Indicators

`.ind` indicators run in Orbit Tape's own interpreter. The language has no
network, file or process access, and each run has memory and work limits
(loops included).
Indicators from the community catalog are checked against the size and SHA-256
checksum listed in its `catalog.json` before they are installed. Review
indicators from other sources before you add them, as you would any script.

## Headless commands and the MCP server

`orbit-tape scan`, `watch`, `mcp` and `report` open no network ports. The
systemd units the .deb ships (scan timer, watcher, report timer) are user
units and stay off until you enable them.

- **`orbit-tape mcp`** speaks MCP over stdin and stdout only, to the client
  that started it. It reads public candles, your indicators and your plan,
  and the only file it writes is `alerts.json`. It cannot place orders and
  holds no exchange keys. A client that can call it can create, change,
  pause and delete your saved alerts, so add it only to clients you trust.
- **`orbit-tape report`** runs the `claude` CLI with this program's MCP
  server alone (`--strict-mcp-config`) and only the six tools the routine
  needs allowed: it can read and create alerts, not change, pause or delete
  them. It runs from the reports folder, so no project's `CLAUDE.md` or
  settings apply.
- **Files:** `alerts.json` (which holds the ntfy topic and maybe a server
  address with a password) and the saved reports are written readable by
  you only (mode 0600).
- **ntfy topics** work like a shared password: anyone who knows the topic
  can read its messages and send to it. Use a long, random topic, or your
  own ntfy server with access control.
