#!/usr/bin/env bash
# PostgreSQL client tools in the db container of the docker compose deployment, as the
# schema owner (default) or the postgres superuser. The password is read from the secrets
# directory on the host, so it is the current one even after scripts/generate-secrets.sh
# rotated it (a running container keeps showing the secret files it was started with).
#
#   sudo scripts/db-client.sh [--superuser] [--tool TOOL] [--dir DIR] [--env-file FILE]
#                             [-- ARGS...]
#
#   --superuser      connect as postgres (break-glass only: routine work, such as
#                    provisioning staff or backups, is done as the schema owner)
#   --tool TOOL      psql (default), pg_dump, pg_dumpall or pg_restore
#   --dir DIR        secrets directory (default: <repo>/secrets)
#   --env-file FILE  compose env file (default: <repo>/deploy/compose.env)
#
#   sudo scripts/db-client.sh                                     # interactive psql
#   sudo scripts/db-client.sh -- -c "SELECT count(*) FROM kyc.clientes"
#   sudo scripts/db-client.sh -- -v ON_ERROR_STOP=1 -f - < change.sql   # a reviewed script
#   sudo scripts/db-client.sh --tool pg_dump -- -Fc > synthetic_capital.dump
#
# Every tool connects over the container's local socket to POSTGRES_DB (-d; -l for pg_dumpall).
# The password reaches it through the environment of `docker compose exec`, never on a
# command line. The role and database names come from the container (DB_OWNER, POSTGRES_DB).
set -euo pipefail

die() { echo "db-client: $*" >&2; exit 1; }

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dir="$repo_root/secrets"
env_file="$repo_root/deploy/compose.env"
superuser=0
tool=psql

while [[ $# -gt 0 ]]; do
  case "$1" in
    --superuser) superuser=1; shift ;;
    --tool) [[ $# -ge 2 ]] || die "--tool needs a value"; tool="$2"; shift 2 ;;
    --dir) [[ $# -ge 2 ]] || die "--dir needs a value"; dir="$2"; shift 2 ;;
    --env-file) [[ $# -ge 2 ]] || die "--env-file needs a value"; env_file="$2"; shift 2 ;;
    --) shift; break ;;
    -h|--help) awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"; exit 0 ;;
    *) die "unknown argument: $1 (arguments for the tool go after --)" ;;
  esac
done

case "$tool" in
  psql) extra='-X -d' ;;          # -X: ignore any ~/.psqlrc
  pg_dump|pg_restore) extra='-d' ;;
  pg_dumpall) extra='-l' ;;       # its -d takes a connection string
  *) die "unknown tool: $tool (psql, pg_dump, pg_dumpall, pg_restore)" ;;
esac
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
# $DB_OWNER and $POSTGRES_DB are escaped: they expand in the container's shell.
exec docker compose --project-directory "$repo_root" --env-file "$env_file" \
  exec "${tty[@]}" -e PGPASSWORD db \
  sh -c "exec $tool -h /var/run/postgresql -U \"$role\" $extra \"\$POSTGRES_DB\" \"\$@\"" "$tool" "$@"
