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
  or returns 404. The only local public content is the informational page at
  `/` and its Misskey wordmark asset.
- **Two narrow exceptions**, both required for correctness, not policy:
  - Three paths (`/notes/:note`, `/users/:user`, `/@:acct`) serve either
    an ActivityPub JSON object or a full HTML page from Misskey, chosen by
    the `Accept` header. This proxy rewrites that header to
    `application/activity+json` on those three paths, so the HTML variant
    is never reachable from the public internet and every caller — a
    federated server, a crawler, a person pasting the URL — gets AP JSON.
    Misskey's client-only feed twins of the acct path (`/@user.rss`,
    `/@user.atom`, `/@user.json`, including percent-encoded spellings) are
    rejected outright: find-my-way routes them to the feeds before the
    ActivityPub route, regardless of `Accept`.
  - `/files/*` and `/proxy/*` (media) redirect to the internal deployment
    instead of proxying bytes when the request's `Referer` looks internal
    — a bandwidth optimization, not a security boundary. It is scoped to
    those media routes only; a spoofed internal `Referer` on any other
    path still gets a plain 404. With `MEDIA_MODE=redirect` these routes
    stop being a relay at all: nothing on them is ever forwarded to
    Misskey. An internal `Referer` is redirected to the internal
    deployment as before, `/proxy/*?url=` is redirected to the original
    URL when it falls under an allowed prefix (`MEDIA_ALLOWED_PREFIXES`)
    and is a 404 otherwise, and `/files/*` without an internal `Referer`
    is a 404. See [`docs/routes.md`](docs/routes.md) for the rules.
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
| `INTERNAL_BASE_URL` | `https://misskey.your-tailnet.ts.net` | Redirect target for internal media callers. With `MEDIA_MODE=redirect` it is checked at startup: an `http(s)` origin only, no userinfo, query, fragment or path |
| `INTERNAL_REFERER_SUFFIX` | `.your-tailnet.ts.net` | Hostname suffix that marks a `Referer` as internal. Required in both modes |
| `MEDIA_MODE` | `proxy` (default), `redirect` | `proxy` relays `/files/*` and `/proxy/*` to Misskey (an internal `Referer` still gets the redirect). `redirect` never forwards them: see [Media mode](#media-mode). An unknown or empty value stops the proxy at startup |
| `MEDIA_ALLOWED_PREFIXES` | `https://misskey.example.com/files/,https://r2.example.net/` | `redirect` mode only (ignored in `proxy` mode, with a `warn` log if `RUST_LOG` lets it through): comma-separated `https://host[:port][/path-prefix]` entries that a `/proxy/*?url=` request may be redirected to. Empty means every such request is a 404. An invalid entry stops the proxy at startup |
| `STATIC_DIR` | `/usr/local/share/misskey-egress-proxy` (default) | Directory containing optional `index.html` and `misskey.svg` overrides for the public landing page |

The image includes a default page at `/`. To replace it without rebuilding,
bind-mount a directory over `/usr/local/share/misskey-egress-proxy` (or set
`STATIC_DIR` to another mounted path). Either file may be omitted; a missing
`index.html` or `misskey.svg` falls back to the version bundled into the binary.
The bundled wordmark is vendored from
[`packages/frontend/assets/misskey.svg`](https://github.com/misskey-dev/misskey/blob/develop/packages/frontend/assets/misskey.svg)
in the official Misskey repository.

See `docker-compose.yml` for a production reference layout.

### Media mode

`MEDIA_MODE=redirect` is for a deployment where something in front of this
proxy answers `/files/*`, and media that is hosted elsewhere (object storage,
a CDN) should be handed to the client instead of relayed. In that mode:

| Request | Result |
|---|---|
| Any `/files/*` or `/proxy/*` with an internal `Referer` | `302` to `INTERNAL_BASE_URL` + its own path and query (same as `proxy` mode) |
| `/proxy/*?url=<URL>` where `<URL>` is under an allowed prefix | `302` to that URL, normalised |
| `/proxy/*?url=<URL>` anywhere else, or malformed | `404` |
| `/files/*` without an internal `Referer` | `404` |

Example, with placeholder hosts:

```
MEDIA_MODE=redirect
INTERNAL_BASE_URL=https://misskey.your-tailnet.ts.net
INTERNAL_REFERER_SUFFIX=.your-tailnet.ts.net
MEDIA_ALLOWED_PREFIXES=https://misskey.example.com/files/,https://r2.example.net/
```

**Read these before turning it on:**

- **`/proxy` no longer processes anything.** It redirects to the original
  file, so Misskey's resizing, webp conversion and still-image variants
  (`static`, `avatar`, `emoji`, `preview`, `badge`) do not happen.
- **Objects behind `MEDIA_ALLOWED_PREFIXES` must be public-read, and signed
  URLs are not supported.** The `Location` is the parsed URL re-serialised,
  which can change a signature in the query, and redirecting to a URL that
  carries an expiring secret would put that secret in the response anyway.
- **On a shared S3 endpoint, name the bucket in the entry**
  (`https://s3.example.com/my-bucket/`). A host-only entry also allows every
  other tenant's bucket on that endpoint. Prefixes match on path-segment
  boundaries, so `/my-bucket/` does not allow `/my-bucket-evil/`. For your
  own domain, always include the `/files/` path.
- **A caller without an internal `Referer` cannot fetch `/files/*` from this
  proxy at all** (it is a `404`). That is meant to be answered by whatever
  sits in front; if that routing is ever wrong, `/files/*` fails loudly
  rather than reaching Misskey.
- **Only `https://` entries are accepted, and only exact host names**: no IP
  addresses, no wildcards, no trailing dot.
- Requests it cannot vouch for are refused rather than repaired: the `url`
  must be a single, plain `https://host[:port]/path` value. The validation
  rules and why each exists are in [`docs/routes.md`](docs/routes.md).

One behaviour change applies to `proxy` mode as well: an internal-`Referer`
redirect is now only sent for a clean path, so a `/files/../x` with an
internal `Referer` is a `404` rather than a `302`. Relaying to Misskey is
untouched.

### Companion project: misskey-files-proxy

In the author's setup two projects share the media traffic. `/files/*` is
routed at the edge (cloudflared, `^/files`) to
[nananek/misskey-files-proxy](https://github.com/nananek/misskey-files-proxy)
(mfp), which answers `302` to Cloudflare R2 for files that have been migrated
and relays the rest to Misskey. Everything else, `/proxy/*` included, reaches
this proxy, which in `redirect` mode `302`s to the allowed original URL
(object storage, or the own domain's `/files/`, which the edge then hands to
mfp).

**The two are tightly coupled.** (1) The edge routing (`^/files` to mfp) is
assumed; without it this proxy's `/files/*` is a `404`. (2) The own-domain
`/files/` entry in `MEDIA_ALLOWED_PREFIXES` assumes mfp is what answers it.
(3) Neither calls the other at run time (mfp's `upstream` is Misskey itself,
not this proxy), so what mfp relays to Misskey for a `/files/:key` it does not
hold is out of this mode's control. (4) mfp is written for its author's
Misskey; it is not a general component.

This describes the author's configuration and is not something the proxy
checks: nothing in the code depends on it, and the behaviour above holds for
whatever values you put in `MEDIA_ALLOWED_PREFIXES`.

## Deployment notes

- **Timeouts and connection limits are the terminator's job.** This
  process has none of its own, by design — cloudflared/Caddy should
  provide request timeouts and concurrency limits. The one bound inside
  the proxy is the rejection drain: a rejected request reads at most
  1 MiB of its body, for at most 10s, before the 404/405 goes out
  (`src/reject.rs`).
- **Replace, don't append, `X-Forwarded-For`** in the terminator. The
  proxy forwards client headers verbatim, and Misskey's default
  `trustProxy` derives `request.ip` from `X-Forwarded-For`, so an
  appending terminator lets a caller pick the IP Misskey records. Today
  only `/api/*` and sign-in rate limits consume it — none reachable
  through this proxy — but replacing the header is the safe setting.
- **Keep `allowedPrivateNetworks` unset in Misskey.** `/proxy/*` is
  public by design (federation needs remote media), and Misskey's own
  SSRF guard is what keeps it from fetching private addresses. With
  `MEDIA_MODE=redirect` it is no longer a public media proxy: it never
  reaches Misskey and never fetches anything itself, it only redirects to
  the prefixes you allow.

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
