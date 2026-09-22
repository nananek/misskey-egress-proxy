#!/bin/sh
# SPDX-FileCopyrightText: misskey-egress-proxy contributors
# SPDX-License-Identifier: AGPL-3.0-only
#
# The "Mitra <-> misskey-egress-proxy <-> Misskey" federation proof. Same
# shape as misskey/scenario.sh, but the peer is a genuinely independent
# ActivityPub implementation (Rust, Mastodon-compatible REST API) instead
# of another Misskey — this is what actually tells us the proxy's Accept
# gate and allowlist work against real-world request shapes in general,
# not just the ones Misskey's own client happens to send.
#
# bob@mitra's OAuth flow (app registration -> password grant) follows the
# exact API nananek/sakurasato's own conftest.py uses against real Mitra.
#
# Generic checks against A's public surface (allowlist / Accept gate /
# media redirect) live in check-a-gate.sh, shared with every other peer.
set -u
. /lib.sh

A_TOKEN="$(cat /tokens/a.token)"

##
## 0. OAuth app registration + password grant for bob@mitra (Mastodon-
##    compatible API; Mastodon itself dropped the password grant, but
##    Mitra, like Pleroma, still has it).
##
note "registering an OAuth app on Mitra"
status="$(retry_status 200 call POST "https://mitra/api/v1/apps" \
	-H 'content-type: application/json' \
	-d '{"client_name":"misskey-egress-proxy-federation-test","redirect_uris":"urn:ietf:wg:oauth:2.0:oob","scopes":"read write follow"}')"
bob_token=""
if [ "$status" = "200" ]; then
	client_id="$(jq -r '.client_id' "$BODY_FILE")"
	client_secret="$(jq -r '.client_secret' "$BODY_FILE")"
	ok "registered OAuth app on Mitra"

	note "getting bob's access token via OAuth password grant"
	status="$(retry_status 200 call POST "https://mitra/oauth/token" \
		-H 'content-type: application/json' \
		-d "{\"grant_type\":\"password\",\"username\":\"bob\",\"password\":\"password123\",\"client_id\":\"${client_id}\",\"client_secret\":\"${client_secret}\",\"scope\":\"read write follow\"}")"
	if [ "$status" = "200" ]; then
		bob_token="$(jq -r '.access_token' "$BODY_FILE")"
		ok "got bob's access token"
	else
		bad "OAuth token request failed (HTTP ${status}): $(cat "$BODY_FILE")"
	fi
else
	bad "OAuth app registration on Mitra failed (HTTP ${status}): $(cat "$BODY_FILE")"
fi

##
## 1. bob@mitra resolves and follows A's admin user through the public
##    proxy. (WebFinger + actor dereference + inbox POST all happen on A's
##    side, routed through misskey-a -> misskey-egress-proxy-a.)
##
remote_id=""
if [ -n "$bob_token" ]; then
	note "resolving admin@misskey-a from Mitra"
	status="$(retry_status 200 call GET "https://mitra/api/v1/accounts/search?q=admin@misskey-a&resolve=true" \
		-H "Authorization: Bearer ${bob_token}")"
	if [ "$status" = "200" ]; then
		remote_id="$(jq -r '.[0].id // empty' "$BODY_FILE")"
		if [ -n "$remote_id" ]; then
			ok "resolved admin@misskey-a via proxy (id=${remote_id})"
		else
			bad "resolve returned 200 but no account: $(cat "$BODY_FILE")"
		fi
	else
		bad "resolving admin@misskey-a failed (HTTP ${status}): $(cat "$BODY_FILE")"
	fi
fi

if [ -n "$remote_id" ]; then
	note "bob@mitra following admin@misskey-a"
	status="$(call POST "https://mitra/api/v1/accounts/${remote_id}/follow" \
		-H "Authorization: Bearer ${bob_token}")"
	case "$status" in
	200) ok "follow accepted by Mitra" ;;
	*) bad "follow failed (HTTP ${status}): $(cat "$BODY_FILE")" ;;
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
## 3. A posts a note; confirm bob@mitra eventually sees it delivered (this
##    leg is A -> Mitra outbound delivery, not through our proxy, but it's
##    the other half of "does federation actually work end-to-end").
##
marker="hello-mitra-$(date +%s%N 2>/dev/null || date +%s)"
note "creating a marked note on A (UDS) to check outbound delivery"
status="$(call_uds_a POST /api/notes/create \
	-H 'content-type: application/json' -H "Authorization: Bearer ${A_TOKEN}" \
	-d "{\"text\":\"${marker}\"}")"
if [ "$status" != "200" ]; then
	bad "notes/create on A failed (HTTP ${status}): $(cat "$BODY_FILE")"
elif [ -n "$bob_token" ]; then
	note "polling Mitra's home timeline for the delivered note"
	seen=0
	attempt=0
	while [ "$seen" -eq 0 ] && [ "$attempt" -lt 30 ]; do
		call GET "https://mitra/api/v1/timelines/home?limit=40" \
			-H "Authorization: Bearer ${bob_token}" >/dev/null
		if jq -e --arg m "$marker" 'any(.[]; (.content // "") | contains($m))' "$BODY_FILE" >/dev/null 2>&1; then
			seen=1
			break
		fi
		attempt=$((attempt + 1))
		sleep 2
	done
	if [ "$seen" -eq 1 ]; then
		ok "bob@mitra's home timeline shows A's note — outbound delivery works"
	else
		bad "bob@mitra never saw A's note delivered"
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
