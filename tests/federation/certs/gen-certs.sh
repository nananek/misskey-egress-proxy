#!/bin/sh
# SPDX-FileCopyrightText: misskey-egress-proxy contributors
# SPDX-License-Identifier: AGPL-3.0-only
set -eu

# Self-signed, per-hostname certs for the federation test's TLS terminators.
# Nothing in this stack validates the certificate chain (both curl's -k and
# Misskey's NODE_TLS_REJECT_UNAUTHORIZED=0 disable verification entirely —
# see docker-compose.yml), so a CA is unnecessary complexity here.

mkdir -p /certs

IFS=','
for domain in $CERT_DOMAINS; do
	openssl req -x509 -newkey rsa:2048 -nodes -days 1 \
		-keyout "/certs/${domain}.key" -out "/certs/${domain}.crt" \
		-subj "/CN=${domain}" \
		-addext "subjectAltName=DNS:${domain}"
done

chmod 644 /certs/*.crt /certs/*.key
echo "certs ready for: ${CERT_DOMAINS}"
