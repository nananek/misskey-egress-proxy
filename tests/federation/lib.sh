#!/bin/sh
# SPDX-FileCopyrightText: misskey-egress-proxy contributors
# SPDX-License-Identifier: AGPL-3.0-only
#
# Shared shell helpers for seed-a.sh and every peer's seed/scenario
# scripts. Sourced, not executed.

# POSTs $3 as JSON to $2 (optionally with an Authorization header via $4,
# and extra curl args via $5, e.g. a --unix-socket target), retrying while
# curl fails to connect at all — the containers we depend on report
# "started"/"healthy" a little before they're actually ready to accept
# connections through nginx's first TLS handshake. Writes the response body
# to $1 and prints the HTTP status code on stdout.
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

# ── Scenario-script helpers ─────────────────────────────────────────
#
# Shared by every peer's scenario.sh and by check-a-gate.sh. A scenario
# script sources this, then check-a-gate.sh at the end, so `fail` and
# `BODY_FILE` stay shared across both without extra plumbing.
#
# Deliberately no `set -e` anywhere these are used: a scenario script's job
# is to run every assertion and report which ones failed, not stop at the
# first one. (`retry_status` returning non-zero after exhausting its
# retries, in particular, must not abort the caller — it must fall through
# to the normal pass/fail check.)

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
	# The `|| echo 000` is load-bearing: a transport-level curl failure
	# (connection refused, TLS handshake, ...) must surface as a retryable
	# "000" status, not abort a `status="$(...)"` assignment.
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
