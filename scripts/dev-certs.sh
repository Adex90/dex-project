#!/usr/bin/env bash
# Self-signed TLS certificate for local and docker compose testing of the nginx edge.
# NOT for production: there, certbot on the host (http-01 through the edge's
# /.well-known/acme-challenge/ location, see nginx/certbot-deploy-hook.sh) or your CA.
#
#   scripts/dev-certs.sh [--out DIR] [--host NAME]... [--days N] [--owner UID:GID] [--force]
#
#   --out DIR        where to write fullchain.pem and privkey.pem (default: <repo>/certs,
#                    mounted read-only at /etc/nginx/certs by docker compose)
#   --host NAME      extra DNS name or IP for the certificate (repeatable). localhost,
#                    127.0.0.1 and ::1 are always included; the first --host is the CN.
#   --days N         validity in days (default 30)
#   --owner UID:GID  owner of the files when run as root (default 101:101, the nginx user
#                    of the container, which must be able to read privkey.pem)
#   --force          replace an existing certificate
#
# Idempotent: an existing certificate is kept (and described) unless --force is given.
set -euo pipefail

usage() { awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"; }
die() { echo "dev-certs: $*" >&2; exit 1; }

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="$repo_root/certs"
days=30
owner="101:101"
force=0
hosts=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) [[ $# -ge 2 ]] || die "--out needs a value"; out="$2"; shift 2 ;;
    --host) [[ $# -ge 2 ]] || die "--host needs a value"; hosts+=("$2"); shift 2 ;;
    --days) [[ $# -ge 2 ]] || die "--days needs a value"; days="$2"; shift 2 ;;
    --owner) [[ $# -ge 2 ]] || die "--owner needs a value"; owner="$2"; shift 2 ;;
    --force) force=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown argument: $1" ;;
  esac
done

[[ "$days" =~ ^[1-9][0-9]{0,3}$ ]] || die "--days must be a number between 1 and 9999"
[[ "$owner" =~ ^[0-9]+:[0-9]+$ ]] || die "--owner must be UID:GID (numeric)"
command -v openssl >/dev/null 2>&1 || die "openssl is required"

describe() {
  openssl x509 -in "$out/fullchain.pem" -noout -subject -enddate -fingerprint -sha256
  openssl x509 -in "$out/fullchain.pem" -noout -text | grep -A1 'Subject Alternative Name' | tail -n1 | sed 's/^ */SAN: /'
}

if [[ -e "$out/fullchain.pem" || -e "$out/privkey.pem" ]] && [[ $force -eq 0 ]]; then
  echo "dev-certs: $out already holds a certificate (use --force to replace it):"
  describe
  exit 0
fi

# Subject alternative names: always the loopback names, plus every --host.
san="DNS:localhost,IP:127.0.0.1,IP:::1"
cn="localhost"
for h in ${hosts[@]+"${hosts[@]}"}; do  # (empty-array safe under set -u on bash 3.2)
  [[ "$h" =~ ^[A-Za-z0-9.:*-]+$ ]] || die "invalid host name: $h"
  if [[ "$h" =~ ^[0-9.]+$ || "$h" == *:* ]]; then san+=",IP:$h"; else san+=",DNS:$h"; fi
done
[[ ${#hosts[@]} -gt 0 ]] && cn="${hosts[0]}"

# The directory must be traversable by the container's uid 101; the key itself is
# protected by its own mode.
if [[ ! -d "$out" ]]; then
  mkdir -p "$out"
  chmod 0755 "$out"
elif [[ ! -x "$out" ]] || [[ "$(find "$out" -maxdepth 0 -perm -o=x)" != "$out" ]]; then
  echo "dev-certs: warning: $out is not traversable by other users; nginx (uid 101) cannot read it" >&2
fi
umask 077
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# A config file instead of -addext keeps this working with OpenSSL 1.1, 3.x and LibreSSL.
cat > "$tmp/openssl.cnf" <<EOF
[req]
distinguished_name = dn
prompt = no
x509_extensions = v3
[dn]
O = Synthetic Capital (development only)
CN = $cn
[v3]
basicConstraints = critical, CA:FALSE
keyUsage = critical, digitalSignature
extendedKeyUsage = serverAuth
subjectAltName = $san
subjectKeyIdentifier = hash
EOF

# ECDSA P-256: the key type browsers and every TLS 1.3 client handle fastest.
openssl req -x509 -new -nodes -sha256 -days "$days" \
  -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
  -config "$tmp/openssl.cnf" \
  -keyout "$tmp/privkey.pem" -out "$tmp/fullchain.pem" 2>/dev/null \
  || die "openssl could not create the certificate"

chmod 0644 "$tmp/fullchain.pem"
if [[ $EUID -eq 0 ]]; then
  # The nginx container runs as uid 101: only that user (and root) can read the key.
  chown "$owner" "$tmp/fullchain.pem" "$tmp/privkey.pem"
  chmod 0400 "$tmp/privkey.pem"
else
  # Without root the key cannot be handed to uid 101; a world-readable key is
  # acceptable only because this is a throwaway self-signed development certificate.
  chmod 0644 "$tmp/privkey.pem"
  echo "dev-certs: not root, so privkey.pem is world-readable (development only)" >&2
fi
mv -f "$tmp/fullchain.pem" "$out/fullchain.pem"
mv -f "$tmp/privkey.pem" "$out/privkey.pem"

echo "dev-certs: wrote $out/fullchain.pem and $out/privkey.pem (self-signed, $days days)"
describe
