#!/bin/sh
# SPDX-FileCopyrightText: misskey-egress-proxy contributors
# SPDX-License-Identifier: AGPL-3.0-only
#
# Generic checks against instance A's public surface: allowlist / Accept
# rewriting / media redirect. None of this is peer-specific — it only exercises
# misskey-a's own public HTTPS endpoint directly — so every peer's
# scenario.sh sources this at the end instead of duplicating it.
#
# Meant to be sourced (after lib.sh) by a scenario.sh that already has
# A_TOKEN, `ok`/`bad`/`note`/`call`/`call_uds_a`/`fail` available.

##
## Create a note on A to exercise the Accept-header gate against.
##
note "creating a note on A (UDS) for the Accept-gate check"
status="$(call_uds_a POST /api/notes/create \
	-H 'content-type: application/json' -H "Authorization: Bearer ${A_TOKEN}" \
	-d '{"text":"hello from A, via misskey-egress-proxy"}')"
gate_note_id=""
if [ "$status" = "200" ]; then
	gate_note_id="$(jq -r '.createdNote.id' "$BODY_FILE")"
	ok "note created on A (id=${gate_note_id})"
else
	bad "notes/create on A failed (HTTP ${status}): $(cat "$BODY_FILE")"
fi

##
## Negative test: everything NOT on the allowlist must be unreachable from
## the public side, no matter how plausible-looking.
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
	"/avatar/@admin" \
	"/@admin.rss" \
	"/@admin.atom" \
	"/@admin.json" \
	"/@admin%2erss"; do
	status="$(call GET "https://misskey-a${path}")"
	if [ "$status" = "404" ]; then
		ok "${path} -> 404 as expected"
	else
		bad "${path} -> HTTP ${status}, expected 404"
	fi
done

##
## Accept rewriting on the dual-purpose note path: whatever the caller asks
## for, a real AP Note comes back and Misskey's HTML branch stays out of
## reach. `-` stands for "send no Accept header at all".
##
if [ -n "$gate_note_id" ]; then
	note "checking that /notes/${gate_note_id} always answers with AP JSON"
	for accept in "-" "text/html" "*/*" "text/html,application/xhtml+xml" "application/activity+json"; do
		if [ "$accept" = "-" ]; then
			label="no Accept"
			status="$(call GET "https://misskey-a/notes/${gate_note_id}")"
		else
			label="Accept: ${accept}"
			status="$(call GET "https://misskey-a/notes/${gate_note_id}" -H "Accept: ${accept}")"
		fi

		if [ "$status" != "200" ]; then
			bad "/notes/${gate_note_id} with ${label} -> HTTP ${status}, expected 200"
			continue
		fi

		note_type="$(jq -r '.type // empty' "$BODY_FILE")"
		if [ "$note_type" = "Note" ]; then
			ok "/notes/${gate_note_id} with ${label} -> 200 with a real AP Note object"
		else
			bad "/notes/${gate_note_id} with ${label} -> 200 but body doesn't look like an AP Note: $(head -c 200 "$BODY_FILE")"
		fi
	done
fi

##
## Media: forwarded for external callers, redirected for internal ones.
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

# The redirect is scoped to the media routes: the same spoofed Referer on a
# path that is not on the allowlist must still be a plain 404, never a 302
# to the internal host.
status="$(curl -sk -o /dev/null -w '%{http_code}' \
	-H 'Referer: https://caller.internal-a.invalid/' \
	"https://misskey-a/api/meta")"
[ "$status" = "404" ] && ok "internal Referer on /api/meta -> 404 (redirect stays scoped to media)" \
	|| bad "internal Referer on /api/meta -> HTTP ${status}, expected 404"
