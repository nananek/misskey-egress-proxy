# Federation tests: real peers ↔ misskey-egress-proxy ↔ Misskey

This is the real end-to-end proof for this project: instance A is a real
Misskey backend (`misskey/misskey:latest`) fronted by this repo's
`misskey-egress-proxy` exactly as in production, federating with a real,
independent peer implementation. If the allowlist, the `Accept` gate, or
the media redirect were wrong, this would fail the same way it would
against a real fediverse peer.

Modeled on how [nananek/sakurasato](https://github.com/nananek/sakurasato)
runs its own federation tests against real Misskey/Mitra/Fedibird/etc. —
same shape (per-side postgres/redis/app/nginx, a shared test CA,
DNS-alias-only networking, no host ports published) and, for Mitra and
Fedibird, adapted directly from their real entrypoint/config files —
adjusted here to test a proxy in front of Misskey rather than a
from-scratch server.

There are three peers, each a separate compose overlay over a shared base
(instance A):

| Peer | What it is | CI | Verified while building this |
|---|---|---|---|
| `misskey` | A second real Misskey instance | every push/PR + nightly | yes |
| `mitra` | Real Mitra (Rust, Mastodon-compatible API) | every push/PR + nightly | yes |
| `fedibird` | Real Fedibird (Mastodon fork, Ruby/Rails) | manual only (`workflow_dispatch`) | **no**, see below |

`misskey` and `mitra` pull prebuilt images and run in a couple of minutes.
`fedibird` has no prebuilt image anywhere, so it builds a full Mastodon
fork from source on every run — 30+ minutes with no cache, matching
sakurasato's own reasoning for keeping it off routine CI. Its compose file
and scripts are a faithful adaptation of sakurasato's own proven
`docker-compose.federation-fedibird.yml`, but — unlike `misskey` and
`mitra` — the build cost meant it was not actually run end to end while
building this project. Treat your first `./run.sh fedibird` as the real
first test of it.

## Running it

```sh
./tests/federation/run.sh <peer>   # misskey | mitra | fedibird
```

Builds and starts the stack for that peer, seeds instance A, runs the full
scenario, prints the seed-a/scenario logs (plus every service's logs on
failure), tears everything down, and exits with the scenario's own exit
code. This is also exactly what CI runs:
[`.github/workflows/ci.yml`](../../.github/workflows/ci.yml) (`misskey` and
`mitra`, on every push/PR to `main` and nightly) and
[`.github/workflows/federation-fedibird.yml`](../../.github/workflows/federation-fedibird.yml)
(`fedibird`, manual only).

For interactive debugging, run the stack manually instead so it's left up
afterwards:

```sh
cd tests/federation
docker compose -f docker-compose.yml -f docker-compose.mitra.yml up --build   # or .misskey. / .fedibird.
docker compose -f docker-compose.yml -f docker-compose.mitra.yml down -v      # clean up when done
```

## What each scenario actually proves

Common to every peer (`peer` below stands for `misskey-b`, `bob@mitra`, or
`bob@fedibird`):

1. **The peer resolves and follows `admin@misskey-a` over its own normal
   public API**, which makes it perform real outbound WebFinger + actor
   dereference + a signed `Follow` POST to A's inbox — all of it addressed
   to `https://misskey-a/`, i.e. through `misskey-a` (nginx TLS
   termination) → `misskey-egress-proxy-a` (this repo) →
   `misskey-app-a` (over its UDS). If the proxy blocked or mangled any of
   `/.well-known/webfinger`, `/users/:id`, or `/users/:id/inbox`, this step
   fails exactly like it would against a real fediverse peer. For `mitra`
   and `fedibird` in particular, this is the point: a genuinely independent
   implementation's HTTP client hitting the proxy, not Misskey talking to
   itself.
2. **A's own `followersCount` (checked via `/api/i` over A's UDS
   directly — the "internal path") becomes 1**, proving the Follow
   genuinely reached and was processed by Misskey through the proxy, not
   just that the peer *thinks* it followed.
3. **A posts a note; the peer eventually sees it delivered** (`misskey-b`'s
   cached `notesCount`, or `bob@{mitra,fedibird}`'s home timeline),
   confirming outbound delivery works too (this leg is A calling out to the
   peer directly, not through the proxy — included for completeness, since
   "federation actually works end-to-end" is what this project is for).

Shared by every peer via [`check-a-gate.sh`](check-a-gate.sh), which only
exercises instance A's own public surface and doesn't care who the peer is:

4. **Everything NOT on the allowlist — `/api/*`, `/streaming`,
   `/oauth/*`, `/healthz`, the well-known client-only paths, `/url`, the
   web client SPA, the `/emoji/*` and `/avatar/*` shorthands — returns
   404 from the public side**, no matter how plausible it looks.
5. **The `Accept` gate on `/notes/:id`** (one of the three dual-purpose
   paths): no `Accept`, or `Accept: text/html`, gets 406; an explicit
   `Accept: application/activity+json` gets a real AP `Note` object back.
6. **Media**: `/files/app-default.jpg` is forwarded through to Misskey
   for an external caller (not blocked — see note below on this specific
   path's own 500), and the same request with an internal-looking
   `Referer` gets a 302 to `INTERNAL_BASE_URL` instead, without ever
   touching Misskey.

## Things worth knowing if you dig into the logs

- **`/files/app-default.jpg` currently 500s on `misskey/misskey:latest`**
  with `ENOENT: .../server/file/assets/dummy.png` — a missing asset in
  the official image itself, reproducible by hitting the UDS directly and
  bypassing this proxy entirely. `check-a-gate.sh` only asserts that the
  proxy *forwarded* the request (status isn't the proxy's own 404); it
  deliberately doesn't assert 200, since that would be asserting Misskey's
  own packaging is correct, which is out of this project's scope.
- **`allowedPrivateNetworks` (Misskey) / `ssrf_protection_enabled: false`
  (Mitra) / `ALLOWED_PRIVATE_ADDRESSES` (Fedibird) are all set.** Every one
  of these implementations refuses outbound requests to private IP ranges
  by default (an SSRF guard) — and this entire "federation" happens on a
  private Docker subnet, so without these overrides a peer would refuse to
  even attempt the WebFinger fetch to A (`Blocked address: ...`). **Never
  set any of these, or `NODE_TLS_REJECT_UNAUTHORIZED=0` (also set on every
  Misskey container here, since its outbound HTTP client doesn't honor
  `NODE_EXTRA_CA_CERTS`), outside a throwaway test network like this one.**
- **`certs/gen-certs.sh` builds a real (if throwaway) CA**, not just
  self-signed leaf certs — required because Mitra and Fedibird properly
  validate TLS (unlike Misskey, which is blinded via
  `NODE_TLS_REJECT_UNAUTHORIZED=0` above). Each peer's entrypoint copies
  `/certs/ca.crt` into its own system trust store.

## Layout

- `run.sh <peer>` — CI's (and your) entry point: up, wait, log, down,
  propagate the exit code.
- `docker-compose.yml` — base stack: instance A only. Not runnable on its
  own — pick a peer overlay.
- `docker-compose.{misskey,mitra,fedibird}.yml` — one overlay per peer,
  bringing up that peer plus the `scenario` service that tests it.
- `lib.sh` — shared shell helpers (`post_json`, `call`, `call_uds_a`,
  `retry_status`, `ok`/`bad`/`note`), sourced by every seed/scenario script.
- `check-a-gate.sh` — the peer-agnostic checks (allowlist / Accept gate /
  media redirect) against instance A, sourced by every peer's
  `scenario.sh`.
- `seed-a.sh` — bootstraps instance A (admin account + enable federation),
  shared across every peer. Goes over A's UDS directly (mirroring how
  `/api/*` is only ever reachable internally in production, never through
  the public egress proxy).
- `config/misskey-a.yml`, `nginx/misskey-a.conf` — instance A, shared.
- `certs/gen-certs.sh` — the shared test CA + per-host certs.
- `misskey/`, `mitra/`, `fedibird/` — each peer's own config, nginx,
  entrypoint, and `scenario.sh`.
