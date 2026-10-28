#!/bin/sh
# certbot deploy hook for the nginx edge: installs a newly issued or renewed certificate
# where docker compose mounts it (/etc/nginx/certs in the container), readable by the
# container's nginx uid only, then reloads nginx gracefully (connections in progress
# finish on the old certificate; a configuration nginx rejects leaves the old one running).
#
# Install on the host, as root:
#   install -m 0755 nginx/certbot-deploy-hook.sh /etc/letsencrypt/renewal-hooks/deploy/synthetic-capital-nginx.sh
# First certificate (the edge serves /.well-known/acme-challenge/ from <deploy dir>/acme,
# so nginx must already run, e.g. with a scripts/dev-certs.sh placeholder certificate):
#   certbot certonly --webroot -w /opt/synthetic-capital/acme -d bank.example.mx
# certbot runs every hook in renewal-hooks/deploy/ after each successful issuance or
# renewal, with RENEWED_LINEAGE=/etc/letsencrypt/live/<name>. Settings (environment):
#   SC_DEPLOY_DIR      compose project directory      (default /opt/synthetic-capital)
#   SC_ENV_FILE        compose env file (default $SC_DEPLOY_DIR/deploy/compose.env; compose
#                      needs it to read docker-compose.yml at all)
#   SC_CERT_DIR        directory mounted at /etc/nginx/certs (default $SC_DEPLOY_DIR/certs)
#   SC_NGINX_UID       uid nginx runs as in the container (default 101; see hardening.sh --nginx-uid)
#   SC_NGINX_SERVICE   compose service name           (default nginx)
set -eu

: "${RENEWED_LINEAGE:?run this as a certbot deploy hook (RENEWED_LINEAGE is not set)}"
deploy_dir="${SC_DEPLOY_DIR:-/opt/synthetic-capital}"
env_file="${SC_ENV_FILE:-$deploy_dir/deploy/compose.env}"
cert_dir="${SC_CERT_DIR:-$deploy_dir/certs}"
nginx_uid="${SC_NGINX_UID:-101}"
service="${SC_NGINX_SERVICE:-nginx}"

umask 077
mkdir -p "$cert_dir"
chmod 0755 "$cert_dir"   # nginx must traverse it; the key protects itself (0400)
tmp="$(mktemp -d "$cert_dir/.incoming.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
# -L: the live/ entries are symlinks into archive/.
cp -L "$RENEWED_LINEAGE/fullchain.pem" "$tmp/fullchain.pem"
cp -L "$RENEWED_LINEAGE/privkey.pem" "$tmp/privkey.pem"
chown "$nginx_uid:$nginx_uid" "$tmp/fullchain.pem" "$tmp/privkey.pem"
chmod 0644 "$tmp/fullchain.pem"
chmod 0400 "$tmp/privkey.pem"
mv -f "$tmp/privkey.pem" "$cert_dir/privkey.pem"
mv -f "$tmp/fullchain.pem" "$cert_dir/fullchain.pem"

# SIGHUP to the nginx master = graceful reload. Compose interpolates the whole file
# before any command, so it needs the env file even to send a signal.
if [ ! -r "$env_file" ]; then
  echo "synthetic-capital: certificate installed in $cert_dir, but $env_file is missing: reload nginx by hand" >&2
  exit 1
fi
if (cd "$deploy_dir" && docker compose --env-file "$env_file" kill -s HUP "$service" >/dev/null); then
  echo "synthetic-capital: certificate for $(basename "$RENEWED_LINEAGE") installed in $cert_dir; nginx reloaded"
else
  echo "synthetic-capital: certificate installed in $cert_dir, but nginx could not be reloaded (docker compose --env-file $env_file kill -s HUP $service)" >&2
  exit 1
fi
