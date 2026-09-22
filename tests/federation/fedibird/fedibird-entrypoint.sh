#!/bin/bash
# SPDX-FileCopyrightText: misskey-egress-proxy contributors
# SPDX-License-Identifier: AGPL-3.0-only
#
# Fedibird entrypoint for the federation test. Taken essentially verbatim
# from nananek/sakurasato's own fedibird-entrypoint.sh: sets up the DB,
# creates a `bob` test account, and — since Fedibird 3.4.1 doesn't support
# the OAuth password grant Mitra/Pleroma still have — issues bob's access
# token directly via Doorkeeper and writes it to a shared volume for
# scenario.sh to read.
set -e

if [ -f /certs/ca.crt ]; then
	cp /certs/ca.crt /usr/local/share/ca-certificates/test-ca.crt
	update-ca-certificates 2>/dev/null || true
fi

until bundle exec ruby -e "require 'pg'; PG.connect(host: ENV['DB_HOST'], port: ENV.fetch('DB_PORT', 5432), user: ENV['DB_USER'], password: ENV['DB_PASS'], dbname: 'postgres')" 2>/dev/null; do
	echo "Waiting for PostgreSQL..."
	sleep 1
done

until bundle exec ruby -e "require 'redis'; Redis.new(host: ENV['REDIS_HOST'], port: ENV.fetch('REDIS_PORT', 6379)).ping" 2>/dev/null; do
	echo "Waiting for Redis..."
	sleep 1
done

echo "Setting up database..."
SAFETY_ASSURED=1 bundle exec rails db:setup 2>/dev/null || bundle exec rails db:migrate

bundle exec rails runner "Setting.registrations_mode = 'open'" 2>/dev/null || true

RAILS_ENV=production bundle exec bin/tootctl accounts create bob --email bob@fedibird --confirmed 2>/dev/null || true
bundle exec rails runner "
  account = Account.find_local('bob')
  if account
    user = account.user
    user.password = 'Password1234!'
    user.save!(validate: false)
  end
" 2>/dev/null || true

echo "Creating OAuth token for bob..."
mkdir -p /tokens
bundle exec rails runner "
  user = Account.find_local('bob')&.user
  if user
    app = Doorkeeper::Application.create_with(
      redirect_uri: 'urn:ietf:wg:oauth:2.0:oob',
      scopes: 'read write follow'
    ).find_or_create_by!(name: 'federation-test')
    token = Doorkeeper::AccessToken.create!(
      application: app,
      resource_owner_id: user.id,
      scopes: 'read write follow',
      expires_in: nil
    )
    File.write('/tokens/bob_token.txt', token.token)
    puts \"Token created: #{token.token[0..7]}...\"
  else
    puts 'ERROR: bob user not found'
    exit 1
  end
" || {
	echo "Failed to create token"
	exit 1
}

exec bundle exec puma -C config/puma.rb
