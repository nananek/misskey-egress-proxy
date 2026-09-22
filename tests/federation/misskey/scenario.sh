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
# Generic checks against A's public surface (allowlist / Accept gate /
# media redirect) live in check-a-gate.sh, shared with every other peer.
set -u
. /lib.sh

A_TOKEN="$(cat /tokens/a.token)"
B_TOKEN="$(cat /tokens/b.token)"

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
note "creating a note on A (UDS) to check outbound delivery"
status="$(call_uds_a POST /api/notes/create \
	-H 'content-type: application/json' -H "Authorization: Bearer ${A_TOKEN}" \
	-d '{"text":"hello from A, via misskey-egress-proxy"}')"
if [ "$status" != "200" ]; then
	bad "notes/create on A failed (HTTP ${status}): $(cat "$BODY_FILE")"
elif [ -n "$remote_id" ]; then
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

. /check-a-gate.sh

echo
if [ "$fail" -eq 0 ]; then
	echo "ALL CHECKS PASSED"
else
	echo "SOME CHECKS FAILED" >&2
fi
exit "$fail"
