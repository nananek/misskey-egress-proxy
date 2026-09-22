#!/bin/sh
# SPDX-FileCopyrightText: misskey-egress-proxy contributors
# SPDX-License-Identifier: AGPL-3.0-only
set -eu

ADMIN_USERNAME="admin"
ADMIN_PASSWORD="AdminTestPass1234"

# POSTs $3 as JSON to $2 (optionally with an Authorization header via $4,
# and extra curl args via $5, e.g. a --unix-socket target), retrying while
# curl fails to connect at all — the containers we depend on report
# "started"/"healthy" a little before they're actually ready to accept
# connections through nginx's first TLS handshake.
post_json() {
	body_file="$1"
	url="$2"
	payload="$3"
	auth_header="${4:-}"
	extra_args="${5:-}"

	attempt=0
	while [ "$attempt" -lt 60 ]; do
		if [ -n "$auth_header" ]; then
			# shellcheck disable=SC2086
			status="$(curl -sk -o "$body_file" -w '%{http_code}' $extra_args \
				-X POST "$url" \
				-H 'content-type: application/json' \
				-H "Authorization: Bearer ${auth_header}" \
				-d "$payload" || echo "000")"
		else
			# shellcheck disable=SC2086
			status="$(curl -sk -o "$body_file" -w '%{http_code}' $extra_args \
				-X POST "$url" \
				-H 'content-type: application/json' \
				-d "$payload" || echo "000")"
		fi

		if [ "$status" != "000" ]; then
			echo "$status"
			return 0
		fi
		attempt=$((attempt + 1))
		sleep 2
	done
	echo "000"
}

# Instance A is the system under test: its `/api/*` admin surface is
# deliberately unreachable through the public egress proxy (that's the
# whole point of this project), so setup goes over the raw UDS instead —
# the same "internal path bypasses the proxy" property production relies
# on, here provided by a direct volume mount instead of Tailscale.
seed_a() {
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
}

# Instance B is a plain remote peer with no proxy in front of it: seeded
# entirely over its normal public HTTPS API, same as any real admin would.
seed_b() {
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
}

seed_a
seed_b

echo "seed complete"
