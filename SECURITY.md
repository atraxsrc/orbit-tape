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
