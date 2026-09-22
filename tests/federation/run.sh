#!/bin/sh
# SPDX-FileCopyrightText: misskey-egress-proxy contributors
# SPDX-License-Identifier: AGPL-3.0-only
#
# Single entry point for the federation test, used by both CI and local
# development: builds and starts the stack for the given peer, waits for
# `scenario` to finish, always dumps the seed-a/scenario logs, dumps every
# service's logs too on failure, tears the stack down, and exits with
# `scenario`'s own exit code.
#
# Usage: ./run.sh <peer>   (peer = misskey | mitra | fedibird)
set -eu
cd "$(dirname "$0")"

peer="${1:?usage: $0 <peer> (misskey|mitra|fedibird)}"
overlay="docker-compose.${peer}.yml"
if [ ! -f "$overlay" ]; then
	echo "no such peer overlay: $overlay" >&2
	exit 1
fi

compose() {
	docker compose -f docker-compose.yml -f "$overlay" "$@"
}

cleanup() {
	compose down -v --remove-orphans >/dev/null 2>&1 || true
}
trap cleanup EXIT

compose up --build -d

cid="$(compose ps -aq scenario)"
exit_code="$(docker wait "$cid")"

echo "::group::seed-a logs"
compose logs seed-a
echo "::endgroup::"

echo "::group::scenario logs"
compose logs scenario
echo "::endgroup::"

if [ "$exit_code" != "0" ]; then
	for svc in $(compose config --services); do
		echo "::group::${svc} logs"
		compose logs "$svc" || true
		echo "::endgroup::"
	done
fi

exit "$exit_code"
