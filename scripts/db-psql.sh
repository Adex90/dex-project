#!/usr/bin/env bash
# psql in the db container of the docker compose deployment, as the schema owner (default)
# or the postgres superuser. The password is read from the secrets directory on the host,
# so it is the current one even after scripts/generate-secrets.sh rotated it (a running
# container keeps showing the secret files it was started with).
#
#   sudo scripts/db-psql.sh [--superuser] [--dir DIR] [--env-file FILE] [-- PSQL_ARGS...]
#
#   --superuser      connect as postgres (break-glass only: everything routine, such as
#                    creating compliance officers, is done as the schema owner)
#   --dir DIR        secrets directory (default: <repo>/secrets)
#   --env-file FILE  compose env file (default: <repo>/deploy/compose.env)
#
#   sudo scripts/db-psql.sh                                        # interactive session
#   sudo scripts/db-psql.sh -- -c "SELECT count(*) FROM kyc.clientes"
#   sudo scripts/db-psql.sh -- -f - < change.sql                   # a reviewed script
#
# The password reaches psql through the environment of `docker compose exec`, never on a
# command line. The role and database names come from the container (DB_OWNER, POSTGRES_DB).
set -euo pipefail

die() { echo "db-psql: $*" >&2; exit 1; }

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dir="$repo_root/secrets"
env_file="$repo_root/deploy/compose.env"
superuser=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --superuser) superuser=1; shift ;;
    --dir) [[ $# -ge 2 ]] || die "--dir needs a value"; dir="$2"; shift 2 ;;
    --env-file) [[ $# -ge 2 ]] || die "--env-file needs a value"; env_file="$2"; shift 2 ;;
    --) shift; break ;;
    -h|--help) awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"; exit 0 ;;
    *) die "unknown argument: $1 (psql arguments go after --)" ;;
  esac
done

if [[ $superuser -eq 1 ]]; then
  secret="$dir/postgres_password"
  role='postgres'
else
  secret="$dir/db_owner_password"
  # shellcheck disable=SC2016 # expanded by the container's shell, which has DB_OWNER
  role='$DB_OWNER'
fi
[[ -r "$secret" ]] || die "cannot read $secret (run as root)"
[[ -r "$env_file" ]] || die "cannot read $env_file"

PGPASSWORD="$(cat "$secret")"
export PGPASSWORD
tty=()
[[ -t 0 && -t 1 ]] || tty=(-T)
# Single quotes: $DB_OWNER and $POSTGRES_DB expand inside the container.
exec docker compose --project-directory "$repo_root" --env-file "$env_file" \
  exec "${tty[@]}" -e PGPASSWORD db \
  sh -c "exec psql -X -h /var/run/postgresql -U \"$role\" -d \"\$POSTGRES_DB\" \"\$@\"" psql "$@"
