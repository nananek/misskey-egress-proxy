#!/bin/sh
# SPDX-FileCopyrightText: misskey-egress-proxy contributors
# SPDX-License-Identifier: AGPL-3.0-only
#
# The actual "Misskey <-> misskey-egress-proxy <-> Misskey" federation
# proof. Everything B does to A goes over A's normal public HTTPS endpoint
# (misskey-a -> misskey-egress-proxy-a -> misskey-app-a): if the proxy's
# allowlist were wrong, this would fail exactly like it would against a
# real federated server on the real internet. State checks on A itself use
# A's UDS directly (the "internal" side of this architecture), the same way
# tests/router.rs mocks things but here against a real Misskey.
#
# Deliberately no `set -e`: this script's job is to run every assertion and
# report which ones failed, not stop at the first one. (`retry_status`
# returning non-zero after exhausting its retries, in particular, must not
# abort the script — it must fall through to the normal pass/fail check.)
set -u

A_TOKEN="$(cat /tokens/a.token)"
B_TOKEN="$(cat /tokens/b.token)"

fail=0
note() { echo "-- $*"; }
ok() { echo "OK   $*"; }
bad() {
	echo "FAIL $*" >&2
	fail=1
}

# GET/POST helpers. `call` returns "<status>" on stdout and leaves the body
# in $BODY_FILE for the caller to inspect.
BODY_FILE=/tmp/resp.json
call() {
	method="$1"
	url="$2"
	shift 2
	# The `|| echo 000` is load-bearing under `set -e`: a transport-level
	# curl failure (connection refused, TLS handshake, ...) must surface as
	# a retryable "000" status, not kill the whole script from inside a
	# `status="$(...)"` assignment.
	curl -sk -o "$BODY_FILE" -w '%{http_code}' -X "$method" "$url" "$@" || echo "000"
}

call_uds_a() {
	method="$1"
	path="$2"
	shift 2
	curl -s -o "$BODY_FILE" -w '%{http_code}' --unix-socket /run/misskey-a/misskey.sock \
		-X "$method" "http://localhost${path}" "$@" || echo "000"
}

retry_status() {
	# retry_status <expected-status-pattern (case pattern)> <cmd...>
	pattern="$1"
	shift
	attempt=0
	while [ "$attempt" -lt 30 ]; do
		status="$("$@")"
		# shellcheck disable=SC2254
		case "$status" in
		$pattern)
			echo "$status"
			return 0
			;;
		esac
		attempt=$((attempt + 1))
		sleep 2
	done
	echo "$status"
	return 1
}

##
## 1. B resolves and follows A's admin user through the public proxy.
##    (WebFinger + actor dereference + inbox POST all happen on A's side,
##    routed through misskey-a -> misskey-egress-proxy-a.)
##
note "resolving admin@misskey-a from instance B"
status="$(retry_status 200 call POST "https://misskey-b/api/users/show" \
	-H 'content-type: application/json' -H "Authorization: Bearer ${B_TOKEN}" \
	-d '{"username":"admin","host":"misskey-a"}')"
if [ "$status" = "200" ]; then
	remote_id="$(jq -r '.id' "$BODY_FILE")"
	ok "resolved admin@misskey-a via proxy (id=${remote_id})"
else
	bad "resolving admin@misskey-a failed (HTTP ${status}): $(cat "$BODY_FILE")"
	remote_id=""
fi

if [ -n "$remote_id" ]; then
	note "instance B following admin@misskey-a"
	status="$(call POST "https://misskey-b/api/following/create" \
		-H 'content-type: application/json' -H "Authorization: Bearer ${B_TOKEN}" \
		-d "{\"userId\":\"${remote_id}\"}")"
	case "$status" in
	200) ok "follow request accepted by B" ;;
	*) bad "following/create failed (HTTP ${status}): $(cat "$BODY_FILE")" ;;
	esac
fi

##
## 2. Confirm A actually received and processed the Follow through the
##    proxy: A's own followersCount (checked over the UDS, i.e. the
##    internal path) must become >= 1.
##
note "polling instance A for followersCount via /api/i (UDS)"
status="$(retry_status 200 call_uds_a POST /api/i \
	-H 'content-type: application/json' -H "Authorization: Bearer ${A_TOKEN}" -d '{}')"
followers_count="$(jq -r '.followersCount // 0' "$BODY_FILE" 2>/dev/null || echo 0)"
attempt=0
while [ "$followers_count" -lt 1 ] && [ "$attempt" -lt 30 ]; do
	sleep 2
	call_uds_a POST /api/i -H 'content-type: application/json' \
		-H "Authorization: Bearer ${A_TOKEN}" -d '{}' >/dev/null
	followers_count="$(jq -r '.followersCount // 0' "$BODY_FILE" 2>/dev/null || echo 0)"
	attempt=$((attempt + 1))
done
if [ "$followers_count" -ge 1 ]; then
	ok "A's followersCount is ${followers_count} — the Follow reached A through misskey-egress-proxy-a"
else
	bad "A's followersCount never reached 1 — the Follow never made it through the proxy"
fi

##
## 3. A posts a note; confirm B eventually sees it delivered (this leg is
##    A -> B outbound delivery, not through our proxy, but it's the other
##    half of "does federation actually work end-to-end").
##
note "creating a note on A (UDS)"
status="$(call_uds_a POST /api/notes/create \
	-H 'content-type: application/json' -H "Authorization: Bearer ${A_TOKEN}" \
	-d '{"text":"hello from A, via misskey-egress-proxy"}')"
note_id=""
if [ "$status" = "200" ]; then
	note_id="$(jq -r '.createdNote.id' "$BODY_FILE")"
	ok "note created on A (id=${note_id})"
else
	bad "notes/create on A failed (HTTP ${status}): $(cat "$BODY_FILE")"
fi

if [ -n "$remote_id" ]; then
	note "polling instance B for delivered note count"
	notes_count=0
	attempt=0
	while [ "$notes_count" -lt 1 ] && [ "$attempt" -lt 30 ]; do
		call POST "https://misskey-b/api/users/show" \
			-H 'content-type: application/json' -H "Authorization: Bearer ${B_TOKEN}" \
			-d "{\"userId\":\"${remote_id}\"}" >/dev/null
		notes_count="$(jq -r '.notesCount // 0' "$BODY_FILE" 2>/dev/null || echo 0)"
		[ "$notes_count" -ge 1 ] && break
		attempt=$((attempt + 1))
		sleep 2
	done
	if [ "$notes_count" -ge 1 ]; then
		ok "B sees ${notes_count} note(s) from A — outbound delivery works"
	else
		bad "B never saw A's note delivered"
	fi
fi

##
## 4. Negative test: everything NOT on the allowlist must be unreachable
##    from the public side, no matter how plausible-looking.
##
note "checking that the client/admin surface is not publicly reachable"
for path in \
	"/api/meta" \
	"/api/v1/instance/peers" \
	"/streaming" \
	"/oauth/token" \
	"/healthz" \
	"/.well-known/oauth-authorization-server" \
	"/.well-known/change-password" \
	"/url?url=https://example.com" \
	"/manifest.json" \
	"/robots.txt" \
	"/" \
	"/emoji/x.webp" \
	"/avatar/@admin"; do
	status="$(call GET "https://misskey-a${path}")"
	if [ "$status" = "404" ]; then
		ok "${path} -> 404 as expected"
	else
		bad "${path} -> HTTP ${status}, expected 404"
	fi
done

##
## 5. Accept-header gate on the dual-purpose note path.
##
if [ -n "$note_id" ]; then
	note "checking Accept-header gate on /notes/${note_id}"
	status="$(call GET "https://misskey-a/notes/${note_id}")"
	[ "$status" = "406" ] && ok "/notes/${note_id} with no Accept -> 406" || bad "/notes/${note_id} with no Accept -> HTTP ${status}, expected 406"

	status="$(call GET "https://misskey-a/notes/${note_id}" -H 'Accept: text/html')"
	[ "$status" = "406" ] && ok "/notes/${note_id} with Accept: text/html -> 406" || bad "/notes/${note_id} with Accept: text/html -> HTTP ${status}, expected 406"

	status="$(call GET "https://misskey-a/notes/${note_id}" -H 'Accept: application/activity+json')"
	if [ "$status" = "200" ]; then
		note_type="$(jq -r '.type // empty' "$BODY_FILE")"
		if [ "$note_type" = "Note" ]; then
			ok "/notes/${note_id} with AP Accept -> 200 with a real AP Note object"
		else
			bad "/notes/${note_id} with AP Accept -> 200 but body doesn't look like an AP Note: $(cat "$BODY_FILE")"
		fi
	else
		bad "/notes/${note_id} with AP Accept -> HTTP ${status}, expected 200"
	fi
fi

##
## 6. Media: forwarded for external callers, redirected for internal ones.
##
note "checking /files/app-default.jpg"
status="$(call GET "https://misskey-a/files/app-default.jpg")"
# Only checking that the proxy let this through to Misskey, not that
# Misskey successfully serves it: the official image can 500 on this exact
# path with ENOENT for its own bundled dummy.png, independent of this
# proxy (reproduced by hitting the UDS directly, bypassing the proxy
# entirely, with the identical error). Our own router's 404 is the only
# response this path could get *from the proxy itself*, so anything else
# means it reached Misskey.
if [ "$status" != "404" ]; then
	ok "/files/app-default.jpg -> HTTP ${status} (reached Misskey through the proxy, not blocked)"
else
	bad "/files/app-default.jpg -> HTTP 404, the proxy blocked a path that should be allowlisted"
fi

status="$(curl -sk -o /dev/null -w '%{http_code}' \
	-H 'Referer: https://caller.internal-a.invalid/' \
	"https://misskey-a/files/app-default.jpg")"
if [ "$status" = "302" ]; then
	location="$(curl -sk -D - -o /dev/null \
		-H 'Referer: https://caller.internal-a.invalid/' \
		"https://misskey-a/files/app-default.jpg" | tr -d '\r' | awk -F': ' 'tolower($1)=="location"{print $2}')"
	case "$location" in
	"http://internal-a.invalid/files/app-default.jpg") ok "internal Referer -> 302 to ${location}" ;;
	*) bad "internal Referer -> 302 but Location was '${location}'" ;;
	esac
else
	bad "internal Referer on /files/app-default.jpg -> HTTP ${status}, expected 302"
fi

echo
if [ "$fail" -eq 0 ]; then
	echo "ALL CHECKS PASSED"
else
	echo "SOME CHECKS FAILED" >&2
fi
exit "$fail"
