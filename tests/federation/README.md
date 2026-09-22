# Federation test: Misskey ↔ misskey-egress-proxy ↔ Misskey

This is the real end-to-end proof for this project: two actual Misskey
instances (`misskey/misskey:latest`), federating with each other, with
instance A fronted by this repo's `misskey-egress-proxy` exactly as in
production. If the allowlist, the `Accept` gate, or the media redirect were
wrong, this would fail the same way it would against a real fediverse peer.

Modeled on how [nananek/sakurasato](https://github.com/nananek/sakurasato)
runs its own federation tests against real Misskey — same shape (per-side
postgres/redis/app/nginx, a shared self-signed test CA, DNS-alias-only
networking, no host ports published), adapted to test a proxy in front of
Misskey rather than a from-scratch server.

## Running it

```sh
./tests/federation/run.sh
```

Builds and starts the stack, seeds an admin account on each instance,
enables federation, runs the full scenario, prints the seed/scenario logs
(plus every service's logs on failure), tears everything down, and exits
with the scenario's own exit code. This is also exactly what CI runs
([`.github/workflows/ci.yml`](../../.github/workflows/ci.yml), on every
push/PR to `main` and nightly).

For interactive debugging, run the stack manually instead so it's left up
afterwards:

```sh
cd tests/federation
docker compose up --build   # Ctrl-C when done watching
docker compose down -v      # clean up (no host ports are published, but
                             # this does use real disk for the Postgres/
                             # Misskey volumes until you do)
```

## What it actually proves

1. **Instance B resolves and follows `admin@misskey-a` over B's normal
   public API**, which makes B's Misskey backend perform real outbound
   WebFinger + actor dereference + a signed `Follow` POST to A's inbox —
   all of it addressed to `https://misskey-a/`, i.e. through
   `misskey-a` (nginx TLS termination) → `misskey-egress-proxy-a` (this
   repo) → `misskey-app-a` (over its UDS). If the proxy blocked or
   mangled any of `/​.well-known/webfinger`, `/users/:id`, or
   `/users/:id/inbox`, this step fails exactly like it would against a
   real fediverse peer.
2. **A's own `followersCount` (checked via `/api/i` over A's UDS
   directly — the "internal path") becomes 1**, proving the Follow
   genuinely reached and was processed by Misskey through the proxy, not
   just that B *thinks* it followed.
3. **A posts a note; B's cached copy of A's `notesCount` becomes 1**,
   confirming outbound delivery works too (this leg is A calling out to
   B directly, not through the proxy — included for completeness, since
   "federation actually works end-to-end" is what this project is for).
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

## Two things worth knowing if you dig into the logs

- **`/files/app-default.jpg` currently 500s on `misskey/misskey:latest`**
  with `ENOENT: .../server/file/assets/dummy.png` — a missing asset in
  the official image itself, reproducible by hitting the UDS directly and
  bypassing this proxy entirely. The scenario only asserts that the proxy
  *forwarded* the request (status isn't the proxy's own 404); it
  deliberately doesn't assert 200, since that would be asserting Misskey's
  own packaging is correct, which is out of this project's scope.
- **`allowedPrivateNetworks` is set in both `config/misskey-*.yml`.**
  Misskey refuses outbound requests to private IP ranges by default (an
  SSRF guard) — and this entire "federation" happens on a private Docker
  subnet, so without this override B's Misskey would refuse to even
  attempt the WebFinger fetch to A (`Blocked address: 172.30.x.x`). The
  compose network is pinned to `172.30.0.0/16` specifically so this
  override can name it reliably. **Never set `allowedPrivateNetworks` or
  `NODE_TLS_REJECT_UNAUTHORIZED=0` (also set here, since Misskey's
  outbound HTTP client doesn't honor `NODE_EXTRA_CA_CERTS` for the
  self-signed test CA) outside a throwaway test network like this one.**

## Layout

- `run.sh` — CI's (and your) entry point: up, wait, log, down, propagate
  the exit code.
- `docker-compose.yml` — the whole stack.
- `config/misskey-{a,b}.yml` — minimal Misskey configs. A listens on a UDS
  (matching production); B listens on a plain TCP port (it's just a
  remote peer, not the system under test).
- `nginx/misskey-{a,b}.conf` — bare TLS termination, nothing else. A's
  forwards to `misskey-egress-proxy-a`; B's goes straight to
  `misskey-app-b`.
- `certs/gen-certs.sh` — self-signed per-host certs (no CA needed: nothing
  in this stack validates the chain anyway, see above).
- `seed.sh` — bootstraps an admin account and enables federation on both
  instances. A's admin bootstrap goes over its UDS directly (mirroring how
  `/api/*` is only ever reachable internally in production); B's goes over
  its normal public API, since B has no proxy in front of it.
- `scenario.sh` — the assertions described above.
