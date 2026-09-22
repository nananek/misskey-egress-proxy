# misskey-egress-proxy

A minimal reverse proxy that sits between the public internet / federated
ActivityPub servers and a [Misskey](https://github.com/misskey-dev/misskey)
instance's Unix domain socket, exposing **only** the routes federation
actually needs — inbox, outbox, actor/note lookup, WebFinger, NodeInfo,
and media — and nothing else.

```
[internal users] - [Tailscale] - [Caddy]           -\
                                                        > [Misskey UDS]
[external users] - [federated AP servers] - [this proxy] -/
```

## Why

Misskey's backend is a single process that mounts everything — the
session/token-authenticated client API (`/api/*`), the WebSocket streaming
endpoint, the OAuth login flow, the admin panel, the full web client SPA,
*and* the ActivityPub federation surface — on one listener. Deployed
as-is, all of that is reachable from the public internet, when in practice
a legitimate logged-in user is fine going through an internal network
(Tailscale + Caddy here), and a federated server only ever needs a small,
well-defined slice of routes.

This project enforces that split at the network edge, since Misskey itself
can't be changed (it's distributed AGPL-3.0-only; we run the official
image unmodified). The approach is directly inspired by
[nananek/sakurasato](https://github.com/nananek/sakurasato), a solo-operator
ActivityPub server that bakes the same idea into its own router — its
public listener registers *only* federation routes, with everything
requiring authentication served from an entirely separate local socket.
Misskey isn't built that way, so this proxy exists to impose the same
shape from the outside.

The full route table — every path this proxy allows through, and why, with
citations into Misskey's own source — is in [`docs/routes.md`](docs/routes.md).

## Design

- **Pure allowlist, no business logic.** The proxy does not parse, join,
  reshape, or cache anything Misskey returns. It matches a request against
  a static route table and either forwards it byte-for-byte (method,
  headers, raw body — this matters for `/inbox`'s HTTP Signature `Digest`)
  or returns 404.
- **Two narrow exceptions**, both required for correctness, not policy:
  - Three paths (`/notes/:note`, `/users/:user`, `/@:acct`) serve either
    an ActivityPub JSON object or a full HTML page from Misskey, chosen by
    the `Accept` header. This proxy rewrites that header to
    `application/activity+json` on those three paths, so the HTML variant
    is never reachable from the public internet and every caller — a
    federated server, a crawler, a person pasting the URL — gets AP JSON.
  - `/files/*` and `/proxy/*` (media) redirect to the internal deployment
    instead of proxying bytes when the request's `Referer` looks internal
    — a bandwidth optimization, not a security boundary.
- **No TLS in this process.** Put a TLS terminator (Caddy, Cloudflare
  Tunnel, ...) in front of it; this binary only ever speaks plain HTTP.
  It can accept that terminator's requests over a Unix socket rather than
  TCP (`LISTEN_ADDR=unix:...`), which lets the proxy container run with
  `network_mode: none` — the only things it can then reach are the two
  sockets it is handed.
- **Rust, AGPL-3.0.** The allowlist is necessarily derived from reading
  Misskey's own (AGPL-3.0-only) source — there's no clean-room public spec
  that fully determines it — so this project ships under the same license
  rather than claiming independence it doesn't have.

## Configuration

All via environment variables (see `src/config.rs`):

| Variable | Example | Meaning |
|---|---|---|
| `LISTEN_ADDR` | `0.0.0.0:8080`, `unix:/run/egress/egress.sock` | Where the proxy itself listens (plain HTTP). A `unix:` prefix listens on a Unix socket instead of TCP |
| `LISTEN_SOCKET_MODE` | `0666` (default) | Mode applied to that socket after `bind`; Unix form only |
| `MISSKEY_SOCKET` | `/run/misskey/misskey.sock` | Misskey's UDS |
| `INTERNAL_BASE_URL` | `https://misskey.your-tailnet.ts.net` | Redirect target for internal media callers |
| `INTERNAL_REFERER_SUFFIX` | `.your-tailnet.ts.net` | Hostname suffix that marks a `Referer` as internal |

See `docker-compose.yml` for a production reference layout.

## Image

CI publishes a built image to GHCR on every push to `main`, after every
test in `ci.yml` passes:

```sh
docker pull ghcr.io/nananek/misskey-egress-proxy:latest
```

Also tagged `sha-<commit>` for pinning to an exact build. Build locally
instead with `docker build .` (see `Dockerfile`) if you'd rather not pull.

## Testing

```sh
cargo test                          # allowlist / Accept-gate / redirect logic, no real Misskey needed
./tests/federation/run.sh misskey   # real Misskey <-> proxy <-> Misskey
./tests/federation/run.sh mitra     # real Mitra <-> proxy <-> Misskey (independent implementation)
./tests/federation/run.sh fedibird  # real Fedibird <-> proxy <-> Misskey (manual only, see below)
```

The federation tests are what actually matter: see
[`tests/federation/README.md`](tests/federation/README.md) for what each
one proves. `cargo test`, `misskey`, and `mitra` all run in CI
([`.github/workflows/ci.yml`](.github/workflows/ci.yml)) on every push/PR
to `main`, plus nightly to catch drift against the real images themselves.
`fedibird` has no prebuilt image and takes 30+ minutes to build from
source, so it's manual-only
([`.github/workflows/federation-fedibird.yml`](.github/workflows/federation-fedibird.yml)).
