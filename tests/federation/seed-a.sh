#!/bin/sh
# SPDX-FileCopyrightText: misskey-egress-proxy contributors
# SPDX-License-Identifier: AGPL-3.0-only
#
# Bootstraps instance A regardless of which peer is under test: creates the
# admin account and enables federation.
#
# Instance A is the system under test: its `/api/*` admin surface is
# deliberately unreachable through the public egress proxy (that's the
# whole point of this project), so setup goes over the raw UDS instead —
# the same "internal path bypasses the proxy" property production relies
# on, here provided by a direct volume mount instead of Tailscale.
set -eu
. /lib.sh

ADMIN_USERNAME="admin"
ADMIN_PASSWORD="AdminTestPass1234"

echo "== seeding instance A (via UDS, bypassing the public egress proxy) =="

body="/tmp/create-a.json"
status="$(post_json "$body" "http://localhost/api/admin/accounts/create" \
	"{\"username\":\"${ADMIN_USERNAME}\",\"password\":\"${ADMIN_PASSWORD}\"}" \
	"" "--unix-socket /run/misskey-a/misskey.sock")"

if [ "$status" != "200" ]; then
	echo "admin creation on A failed (HTTP ${status}): $(cat "$body")" >&2
	exit 1
fi

token="$(jq -r '.token' "$body")"
if [ -z "$token" ] || [ "$token" = "null" ]; then
	echo "no token in admin create response from A: $(cat "$body")" >&2
	exit 1
fi
echo "$token" >/tokens/a.token
echo "admin created on A"

fed_body="/tmp/federation-a.json"
fed_status="$(post_json "$fed_body" "http://localhost/api/admin/update-meta" \
	'{"federation":"all"}' "$token" "--unix-socket /run/misskey-a/misskey.sock")"

case "$fed_status" in
2??) echo "federation enabled on A" ;;
*)
	echo "enabling federation on A failed (HTTP ${fed_status}): $(cat "$fed_body")" >&2
	exit 1
	;;
esac

echo "seed-a complete"
