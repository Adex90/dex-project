#!/usr/bin/env bash
# Generates the Docker secrets files used by docker-compose.yml (single-host deployment).
# Values are random and never printed. Existing files are kept unless rotated.
#
#   sudo scripts/generate-secrets.sh [--dir DIR] [--env-file FILE] [--rotate LIST]
#                                    [--clear-previous LIST] [--dev]
#
#   --dir DIR        output directory (default: <repo>/secrets; keep it out of git and
#                    equal to SECRETS_DIR in the env file)
#   --env-file FILE  where DB_OWNER and POSTGRES_DB come from (default:
#                    <repo>/deploy/compose.env if it exists). As in docker compose, a
#                    variable exported in the shell wins over the file.
#   --rotate LIST    comma-separated secrets to regenerate:
#                      app        sc_app password (+ database_url). Then, in a quiet window,
#                                 recreate migrate (it sets the new password) and the backend.
#                      jwt        JWT signing key (signs every session out)
#                      spei, kyc  webhook HMAC secrets. The replaced value moves to
#                                 *_previous, which the backend keeps accepting
#                                 (SPEI/KYC_WEBHOOK_SECRETO_ANTERIOR) until --clear-previous.
#                      owner      schema owner password (+ database_url_owner)
#                      superuser  postgres superuser password (break-glass only)
#                    owner and superuser also live in the database: when its volume exists,
#                    the db container must be running, and the new password is set there
#                    first (psql \password, authenticated with the old one).
#   --clear-previous LIST   spei and/or kyc: empties *_previous once the provider signs
#                    with the new secret
#   --dev            local testing without root: files are 0444 instead of being owned by
#                    the container users (world-readable on this host; never in production)
#
# Production (as root): every file is mode 0400 and owned by the uid that reads it
# inside its container: 70 (postgres) for postgres_password and db_owner_password,
# 65532 (backend, migrate) for the rest. The directory is 0700. Re-running the script
# restores those permissions on the files it keeps. The bootstrap passwords are only
# used when the migrate job first creates those accounts.
set -euo pipefail
shopt -s inherit_errexit

usage() { awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"; }
die() { echo "generate-secrets: $*" >&2; exit 1; }
info() { echo "generate-secrets: $*"; }

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dir="$repo_root/secrets"
env_file=""
rotate=""
clear_previous=""
dev=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dir) [[ $# -ge 2 ]] || die "--dir needs a value"; dir="$2"; shift 2 ;;
    --env-file) [[ $# -ge 2 ]] || die "--env-file needs a value"; env_file="$2"; shift 2 ;;
    --rotate) [[ $# -ge 2 ]] || die "--rotate needs a value"; rotate="$2"; shift 2 ;;
    --clear-previous) [[ $# -ge 2 ]] || die "--clear-previous needs a value"; clear_previous="$2"; shift 2 ;;
    --dev) dev=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown argument: $1" ;;
  esac
done

if [[ -z "$env_file" && -f "$repo_root/deploy/compose.env" ]]; then
  env_file="$repo_root/deploy/compose.env"
fi

# KEY as docker compose resolves it: the shell environment, then the env file (the last
# assignment wins; quotes and trailing " # comments" are stripped), then DEFAULT. The
# file is parsed, never executed.
env_value() {
  local key="$1" default="$2" line value=""
  if [[ -n "${!key:-}" ]]; then
    printf '%s' "${!key}"
    return
  fi
  if [[ -n "$env_file" ]]; then
    [[ -r "$env_file" ]] || die "cannot read $env_file"
    while IFS= read -r line || [[ -n "$line" ]]; do
      if [[ "$line" =~ ^[[:space:]]*(export[[:space:]]+)?${key}[[:space:]]*=[[:space:]]*(.*)$ ]]; then
        value="${BASH_REMATCH[2]}"
        if [[ "$value" =~ ^\"([^\"]*)\" || "$value" =~ ^\'([^\']*)\' ]]; then
          value="${BASH_REMATCH[1]}"
        else
          value="${value%%[[:space:]]#*}"
          value="${value%"${value##*[![:space:]]}"}"
        fi
      fi
    done < "$env_file"
  fi
  printf '%s' "${value:-$default}"
}

owner_user="$(env_value DB_OWNER synthetic_owner)"
db_name="$(env_value POSTGRES_DB synthetic_capital)"
superuser="postgres"
project="${COMPOSE_PROJECT_NAME:-synthetic-capital}"
db_host="db"
app_user="sc_app"

[[ "$owner_user" =~ ^[a-z_][a-z0-9_]{0,62}$ ]] || die "DB_OWNER must match [a-z_][a-z0-9_]*: $owner_user"
[[ "$db_name" =~ ^[a-z_][a-z0-9_]{0,62}$ ]] || die "POSTGRES_DB must match [a-z_][a-z0-9_]*: $db_name"
case "$owner_user" in
  "$superuser"|pg_*|sc_*) die "DB_OWNER cannot be $owner_user (the superuser, a reserved name or an application role)" ;;
  *) ;;
esac

if [[ $dev -eq 0 && $EUID -ne 0 ]]; then
  die "run as root (the files must be owned by the container users), or pass --dev for local testing"
fi

declare -A rotating=() clearing=()
if [[ -n "$rotate" ]]; then
  IFS=',' read -r -a items <<<"$rotate"
  for item in "${items[@]}"; do
    case "$item" in
      app|jwt|spei|kyc|owner|superuser) rotating[$item]=1 ;;
      *) die "unknown secret to rotate: $item (app, jwt, spei, kyc, owner, superuser)" ;;
    esac
  done
fi
if [[ -n "$clear_previous" ]]; then
  IFS=',' read -r -a items <<<"$clear_previous"
  for item in "${items[@]}"; do
    case "$item" in
      spei|kyc) [[ -z "${rotating[$item]:-}" ]] || die "--rotate $item and --clear-previous $item together would drop the live secret's predecessor at once; run them separately"
                clearing[$item]=1 ;;
      *) die "unknown previous secret to clear: $item (spei, kyc)" ;;
    esac
  done
fi

# Files, in display order, and the uid that reads each one inside its container.
files=(postgres_password db_owner_password database_url_owner db_app_password database_url
       jwt_secret spei_webhook_secret spei_webhook_secret_previous kyc_webhook_secret
       kyc_webhook_secret_previous bootstrap_operador_password bootstrap_cumplimiento_password)
declare -A uid_of=()
for name in "${files[@]}"; do uid_of[$name]=65532; done
uid_of[postgres_password]=70
uid_of[db_owner_password]=70

# Hex: high entropy, no URL encoding needed, accepted by every validator in the backend.
random_hex() {
  local bytes="$1" value
  if command -v openssl >/dev/null 2>&1; then
    value="$(openssl rand -hex "$bytes")"
  else
    value="$(head -c "$bytes" /dev/urandom | od -An -tx1 -v | tr -d ' \n')"
  fi
  [[ ${#value} -eq $((bytes * 2)) && "$value" =~ ^[0-9a-f]+$ ]] || die "random generation failed"
  printf '%s' "$value"
}

protect() { # protect FILE UID: 0400 owned by UID, or 0444 with --dev
  if [[ $dev -eq 1 ]]; then
    chmod 0444 "$1"
  else
    chown "$2:$2" "$1"
    chmod 0400 "$1"
  fi
}

umask 077
mkdir -p "$dir"
chmod 0700 "$dir"

# A temporary file holding a secret that is not in place yet; removed on any exit.
pending=""
trap 'if [[ -n "$pending" ]]; then rm -f "$pending"; fi' EXIT

stage() { # stage NAME VALUE: writes VALUE to a protected temporary file ($pending)
  pending="$(mktemp "$dir/.${1}.XXXXXX")"
  printf '%s' "$2" > "$pending"
  protect "$pending" "${uid_of[$1]}"
}
commit() { # commit NAME: moves the staged file into place (atomic)
  mv -f "$pending" "$dir/$1"
  pending=""
}
write_secret() { stage "$1" "$2"; commit "$1"; }
read_secret() { cat "$dir/$1"; }

declare -A estado=()

ensure() { # ensure NAME BYTES ROTATION_KEY: generates the file if missing, empty or rotated
  local name="$1" bytes="$2" key="$3" value
  if [[ ! -s "$dir/$name" || -n "${rotating[$key]:-}" ]]; then
    value="$(random_hex "$bytes")"
    write_secret "$name" "$value"
    estado[$name]=generated
  else
    estado[$name]=kept
  fi
}

# Whether the database volume exists, i.e. the cluster already holds the passwords.
volume_exists() {
  command -v docker >/dev/null 2>&1 || return 1
  docker info >/dev/null 2>&1 || die "docker is not reachable: cannot tell whether the database volume ${project}_pgdata exists"
  docker volume inspect "${project}_pgdata" >/dev/null 2>&1
}

# db_password NAME ROLE ROTATION_KEY: the superuser and owner passwords also live in the
# cluster (PostgreSQL reads POSTGRES_PASSWORD_FILE, and the init script the owner's file,
# only when an empty volume is initialised). A new value is set there before the file is
# replaced, so the files never name a password the cluster does not have.
db_password() {
  local name="$1" role="$2" key="$3" value old cid output
  if [[ -s "$dir/$name" && -z "${rotating[$key]:-}" ]]; then
    estado[$name]=kept
    return
  fi
  value="$(random_hex 32)"
  stage "$name" "$value"
  if volume_exists; then
    [[ -s "$dir/$name" ]] || die "the volume ${project}_pgdata exists but $dir/$name is missing: restore the secrets directory from its backup"
    cid="$(docker ps -q --filter "label=com.docker.compose.project=${project}" \
                       --filter "label=com.docker.compose.service=db")"
    [[ -n "$cid" ]] || die "the volume ${project}_pgdata exists: start the database first (docker compose --env-file ... up -d db), so the new $role password can be set in it"
    old="$(read_secret "$name")"
    # psql reads its prompts (current password, new password twice) from stdin when there
    # is no terminal; \password sends a SCRAM verifier, never the password itself.
    if ! output="$(printf '%s\n%s\n%s\n' "$old" "$value" "$value" \
        | docker exec -i "$cid" psql -X -q -h /var/run/postgresql -U "$role" -d "$db_name" \
            -c "\\password ${role}" 2>&1)"; then
      die "could not set the new $role password in the database (nothing changed): ${output}"
    fi
    info "$role password changed in the database (container ${cid:0:12})"
  fi
  commit "$name"
  estado[$name]=generated
}

db_password postgres_password "$superuser" superuser
db_password db_owner_password "$owner_user" owner
ensure db_app_password 32 app
ensure jwt_secret 64 jwt

# Webhook secrets: on rotation the replaced value becomes *_previous, which the backend
# still accepts, so deliveries signed by the provider keep verifying while it switches.
# An empty *_previous file means there is none.
for key in spei kyc; do
  name="${key}_webhook_secret"
  if [[ -n "${rotating[$key]:-}" && -s "$dir/$name" ]]; then
    old="$(read_secret "$name")"
    write_secret "${name}_previous" "$old"
    estado[${name}_previous]="holds the replaced secret"
  elif [[ -n "${clearing[$key]:-}" || ! -e "$dir/${name}_previous" ]]; then
    write_secret "${name}_previous" ""
    estado[${name}_previous]="empty"
  else
    estado[${name}_previous]=kept
  fi
  ensure "$name" 32 "$key"
done

ensure bootstrap_operador_password 24 none
ensure bootstrap_cumplimiento_password 24 none

# Connection URLs are derived from the passwords, so they are rebuilt whenever a password
# changed (or the database name, owner or file is missing). Inside the internal `data`
# network the traffic never leaves the host: sslmode=disable (see DEPLOYMENT.md).
owner_password="$(read_secret db_owner_password)"
app_password="$(read_secret db_app_password)"
declare -A url=(
  [database_url_owner]="postgres://${owner_user}:${owner_password}@${db_host}:5432/${db_name}?sslmode=disable"
  [database_url]="postgres://${app_user}:${app_password}@${db_host}:5432/${db_name}?sslmode=disable"
)
for name in database_url_owner database_url; do
  current=""
  if [[ -s "$dir/$name" ]]; then current="$(read_secret "$name")"; fi
  if [[ "$current" != "${url[$name]}" ]]; then
    write_secret "$name" "${url[$name]}"
    estado[$name]=generated
  else
    estado[$name]=kept
  fi
done

# Kept files get their ownership and mode back (e.g. after a copy or a --dev run).
for name in "${files[@]}"; do
  protect "$dir/$name" "${uid_of[$name]}"
  info "$(printf '%-32s %s' "$name" "${estado[$name]}")"
done
if [[ $dev -eq 1 ]]; then
  info "directory: $dir (DEV MODE: files are world-readable)"
else
  info "directory: $dir (files 0400, owned by uid 70 / 65532)"
fi

compose="docker compose --env-file ${env_file:-<env file>}"
if [[ -n "${rotating[app]:-}" ]]; then
  info "sc_app password changed. In a quiet window, run: $compose up -d --force-recreate migrate backend"
fi
if [[ -n "${rotating[jwt]:-}${rotating[spei]:-}${rotating[kyc]:-}" ]]; then
  info "apply the new secrets with: $compose up -d --force-recreate backend"
fi
webhooks=""
for key in spei kyc; do
  if [[ -n "${rotating[$key]:-}" ]]; then webhooks+="${webhooks:+,}$key"; fi
done
if [[ -n "$webhooks" ]]; then
  info "give the provider its new webhook secret; once it signs with it, run $0 --clear-previous $webhooks and recreate the backend"
fi
if [[ -n "${clearing[spei]:-}${clearing[kyc]:-}" ]]; then
  info "previous webhook secret cleared: $compose up -d --force-recreate backend"
fi
