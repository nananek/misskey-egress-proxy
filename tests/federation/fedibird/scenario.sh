#!/bin/sh
# SPDX-FileCopyrightText: misskey-egress-proxy contributors
# SPDX-License-Identifier: AGPL-3.0-only
#
# The "Fedibird <-> misskey-egress-proxy <-> Misskey" federation proof.
# Same shape as mitra/scenario.sh, but Fedibird (a real Mastodon fork) has
# no OAuth password grant, so bob@fedibird's access token is pre-baked by
# fedibird-entrypoint.sh into a shared volume instead of being fetched at
# scenario time.
#
# Generic checks against A's public surface (allowlist / Accept gate /
# media redirect) live in check-a-gate.sh, shared with every other peer.
set -u
. /lib.sh

A_TOKEN="$(cat /tokens/a.token)"

##
## 0. Read bob@fedibird's pre-baked access token.
##
note "reading bob@fedibird's access token"
bob_token=""
attempt=0
while [ -z "$bob_token" ] && [ "$attempt" -lt 30 ]; do
	if [ -s /fedibird-tokens/bob_token.txt ]; then
		bob_token="$(cat /fedibird-tokens/bob_token.txt)"
		break
	fi
	attempt=$((attempt + 1))
	sleep 2
done
if [ -n "$bob_token" ]; then
	ok "got bob@fedibird's access token"
else
	bad "bob@fedibird's access token never appeared at /fedibird-tokens/bob_token.txt"
fi

##
## 1. bob@fedibird resolves and follows A's admin user through the public
##    proxy. (WebFinger + actor dereference + inbox POST all happen on A's
##    side, routed through misskey-a -> misskey-egress-proxy-a.)
##
remote_id=""
if [ -n "$bob_token" ]; then
	note "resolving admin@misskey-a from Fedibird"
	status="$(retry_status 200 call GET "https://fedibird/api/v1/accounts/search?q=admin@misskey-a&resolve=true" \
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
	note "bob@fedibird following admin@misskey-a"
	status="$(call POST "https://fedibird/api/v1/accounts/${remote_id}/follow" \
		-H "Authorization: Bearer ${bob_token}")"
	case "$status" in
	200) ok "follow accepted by Fedibird" ;;
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
## 3. A posts a note; confirm bob@fedibird eventually sees it delivered
##    (this leg is A -> Fedibird outbound delivery, not through our proxy,
##    but it's the other half of "does federation actually work
##    end-to-end").
##
marker="hello-fedibird-$(date +%s%N 2>/dev/null || date +%s)"
note "creating a marked note on A (UDS) to check outbound delivery"
status="$(call_uds_a POST /api/notes/create \
	-H 'content-type: application/json' -H "Authorization: Bearer ${A_TOKEN}" \
	-d "{\"text\":\"${marker}\"}")"
if [ "$status" != "200" ]; then
	bad "notes/create on A failed (HTTP ${status}): $(cat "$BODY_FILE")"
elif [ -n "$bob_token" ]; then
	note "polling Fedibird's home timeline for the delivered note"
	seen=0
	attempt=0
	while [ "$seen" -eq 0 ] && [ "$attempt" -lt 30 ]; do
		call GET "https://fedibird/api/v1/timelines/home?limit=40" \
			-H "Authorization: Bearer ${bob_token}" >/dev/null
		if jq -e --arg m "$marker" 'any(.[]; (.content // "") | contains($m))' "$BODY_FILE" >/dev/null 2>&1; then
			seen=1
			break
		fi
		attempt=$((attempt + 1))
		sleep 2
	done
	if [ "$seen" -eq 1 ]; then
		ok "bob@fedibird's home timeline shows A's note — outbound delivery works"
	else
		bad "bob@fedibird never saw A's note delivered"
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
