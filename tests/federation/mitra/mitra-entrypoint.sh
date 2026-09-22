#!/bin/sh
# SPDX-FileCopyrightText: misskey-egress-proxy contributors
# SPDX-License-Identifier: AGPL-3.0-only
#
# Mitra entrypoint for the federation test: trusts the shared test CA (so
# Mitra's own outbound requests to https://misskey-a/ verify correctly —
# unlike Misskey/undici, Mitra's Rust HTTP client does honor the system
# trust store), creates the `bob` test account, and starts the server.
# Modeled on nananek/sakurasato's own mitra-entrypoint.sh.
set -e

if [ -f /certs/ca.crt ]; then
	cp /certs/ca.crt /usr/local/share/ca-certificates/test-federation-ca.crt
	update-ca-certificates 2>/dev/null || true
	echo "Added test CA cert to trust store"
fi

mkdir -p /var/lib/mitra/www

/app/mitra create-account bob password123 user || true
echo "Created test user bob"

exec /app/mitra server
