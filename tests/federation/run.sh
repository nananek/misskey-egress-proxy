#!/bin/sh
# SPDX-FileCopyrightText: misskey-egress-proxy contributors
# SPDX-License-Identifier: AGPL-3.0-only
#
# Single entry point for the federation test, used by both CI and local
# development: builds and starts the stack, waits for `scenario` to finish,
# always dumps the seed/scenario logs, dumps everything else too on
# failure, tears the stack down, and exits with `scenario`'s own exit code.
set -eu
cd "$(dirname "$0")"

cleanup() {
	docker compose down -v --remove-orphans >/dev/null 2>&1 || true
}
trap cleanup EXIT

docker compose up --build -d

cid="$(docker compose ps -aq scenario)"
exit_code="$(docker wait "$cid")"

echo "::group::seed logs"
docker compose logs seed
echo "::endgroup::"

echo "::group::scenario logs"
docker compose logs scenario
echo "::endgroup::"

if [ "$exit_code" != "0" ]; then
	for svc in misskey-egress-proxy-a misskey-app-a misskey-app-b misskey-a misskey-b; do
		echo "::group::${svc} logs"
		docker compose logs "$svc" || true
		echo "::endgroup::"
	done
fi

exit "$exit_code"
