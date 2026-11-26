#!/usr/bin/env bash
set -euo pipefail

export CARGO_TARGET_DIR=/target CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0
cargo build --locked -p apell-store --bins

umask 077
mkdir -p /data/store /data/relay
cat /etc/ssl/certs/ca-certificates.crt /config/tls/ca.crt > /tmp/apell-local-trust.pem
export SSL_CERT_FILE=/tmp/apell-local-trust.pem
export APELL_DEV_TLS_DIR=/config/tls
export NO_PROXY="localhost,127.0.0.1,::1${NO_PROXY:+,$NO_PROXY}"
export no_proxy="$NO_PROXY"

APELL_STORE_LISTEN=0.0.0.0:8789 \
  APELL_STORE_DATA_DIR=/data/store \
  APELL_STORE_WEB_DIR=/workspace/store/web \
  APELL_STORE_PUBLISH_TOKEN_FILE=/config/publish-token \
  APELL_STORE_PUBLIC_URL=https://localhost:8788 \
  APELL_STORE_ACCOUNT_OIDC_FILE=/config/store-oidc.json \
  APELL_STORE_CONFIG_SYNC_FILE=/config/store-sync.json \
  /target/debug/apell-store &
store=$!

APELL_ACCOUNT_KEY_EXCHANGE_LISTEN=127.0.0.1:5657 \
  APELL_ACCOUNT_KEY_EXCHANGE_DATA_DIR=/data/relay \
  APELL_ACCOUNT_KEY_EXCHANGE_FILE=/config/relay-oidc.json \
  APELL_ACCOUNT_KEY_EXCHANGE_MASTER_KEY_FILE=/config/master.key \
  APELL_ACCOUNT_KEY_EXCHANGE_RECOVERY_OIDC_FILE=/config/recovery-oidc.json \
  /target/debug/apell-account-keys &
relay=$!

bun /workspace/deploy/local/identity.ts &
identity=$!

stop() {
  kill -TERM "$store" "$relay" "$identity" 2>/dev/null || true
  wait "$store" "$relay" "$identity" 2>/dev/null || true
}
trap 'stop; exit 0' TERM INT
trap stop EXIT
wait -n "$store" "$relay" "$identity"
