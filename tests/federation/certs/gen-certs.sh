#!/bin/sh
# SPDX-FileCopyrightText: misskey-egress-proxy contributors
# SPDX-License-Identifier: AGPL-3.0-only
#
# Generate a self-signed CA and per-host server certs for the federation
# test's TLS terminators. Based on nananek/sakurasato's own
# compose/federation-test/_common/gen-certs.sh.
#
# A real CA (not just self-signed leaf certs) matters here: Misskey's own
# outbound HTTP client is blinded to certificate errors by
# NODE_TLS_REJECT_UNAUTHORIZED=0, but peers like Mitra properly validate
# TLS and only get to trust misskey-a's cert by trusting this CA (each
# peer's entrypoint copies /certs/ca.crt into its own system trust store).
#
# Driven by env var CERT_DOMAINS (comma-separated hostnames). Each domain
# gets a cert at /certs/<domain>.{crt,key} signed by /certs/ca.crt. The CA
# itself is generated once and reused across reruns.
#
# Validity is intentionally 1 day — these certs only live as long as the
# compose stack and must never escape the test network.
set -eu

CERT_DIR=/certs
mkdir -p "$CERT_DIR"

if [ -z "${CERT_DOMAINS:-}" ]; then
	echo "CERT_DOMAINS must be set (comma-separated hostnames)" >&2
	exit 1
fi

if [ ! -f "$CERT_DIR/ca.crt" ]; then
	# `keyUsage` / `basicConstraints` must be set explicitly: newer OpenSSL/
	# Python-ssl-style validators reject a CA cert that lacks a keyUsage
	# extension, and `-x509` alone doesn't add one.
	openssl req -x509 -newkey rsa:2048 -nodes \
		-keyout "$CERT_DIR/ca.key" \
		-out "$CERT_DIR/ca.crt" \
		-days 1 \
		-subj "/CN=misskey-egress-proxy Federation Test CA" \
		-addext "basicConstraints = critical, CA:TRUE" \
		-addext "keyUsage = critical, keyCertSign, cRLSign" \
		2>/dev/null
	echo "Generated CA cert"
fi

IFS=','
for domain in $CERT_DOMAINS; do
	if [ -f "$CERT_DIR/$domain.crt" ]; then
		continue
	fi
	openssl req -newkey rsa:2048 -nodes \
		-keyout "$CERT_DIR/$domain.key" \
		-out "$CERT_DIR/$domain.csr" \
		-subj "/CN=$domain" \
		-addext "subjectAltName=DNS:$domain" \
		2>/dev/null
	openssl x509 -req \
		-in "$CERT_DIR/$domain.csr" \
		-CA "$CERT_DIR/ca.crt" \
		-CAkey "$CERT_DIR/ca.key" \
		-CAcreateserial \
		-out "$CERT_DIR/$domain.crt" \
		-days 1 \
		-copy_extensions copyall \
		2>/dev/null
	rm -f "$CERT_DIR/$domain.csr"
	echo "Generated cert for $domain"
done

chmod 644 "$CERT_DIR"/*.crt "$CERT_DIR"/*.key
echo "certs ready for: ${CERT_DOMAINS}"
