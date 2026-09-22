#!/bin/sh
# SPDX-FileCopyrightText: misskey-egress-proxy contributors
# SPDX-License-Identifier: AGPL-3.0-only
#
# Instance B is a plain remote peer with no proxy in front of it: seeded
# entirely over its normal public HTTPS API, same as any real admin would.
set -eu
. /lib.sh

ADMIN_USERNAME="admin"
ADMIN_PASSWORD="AdminTestPass1234"

echo "== seeding instance B (public HTTPS) =="

body="/tmp/create-b.json"
status="$(post_json "$body" "https://misskey-b/api/admin/accounts/create" \
	"{\"username\":\"${ADMIN_USERNAME}\",\"password\":\"${ADMIN_PASSWORD}\"}")"

if [ "$status" != "200" ]; then
	echo "admin creation on B failed (HTTP ${status}): $(cat "$body")" >&2
	exit 1
fi

token="$(jq -r '.token' "$body")"
if [ -z "$token" ] || [ "$token" = "null" ]; then
	echo "no token in admin create response from B: $(cat "$body")" >&2
	exit 1
fi
echo "$token" >/tokens/b.token
echo "admin created on B"

fed_body="/tmp/federation-b.json"
fed_status="$(post_json "$fed_body" "https://misskey-b/api/admin/update-meta" \
	'{"federation":"all"}' "$token")"

case "$fed_status" in
2??) echo "federation enabled on B" ;;
*)
	echo "enabling federation on B failed (HTTP ${fed_status}): $(cat "$fed_body")" >&2
	exit 1
	;;
esac

echo "seed-b complete"
