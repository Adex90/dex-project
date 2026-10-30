#!/bin/sh
# Schema owner for the docker compose deployment. The official postgres image runs this
# once, when it initialises an empty data volume (/docker-entrypoint-initdb.d, as the
# postgres user, against a temporary server on the local socket).
#
# It creates DB_OWNER, the role the one-shot migrate job connects as: LOGIN and
# CREATEROLE (the migrations create the sc_* roles and the sc_app login) but not a
# superuser, so it cannot read server files, run programs or skip row-level security
# on tables it does not own. It owns POSTGRES_DB. The superuser (POSTGRES_USER) is
# kept for break-glass administration and is used by nothing else.
#
# The password is read from DB_OWNER_PASSWORD_FILE and stored as a SCRAM verifier
# computed by psql (\password reads it from stdin when there is no terminal), so the
# plaintext is never part of an SQL statement.
#
# Executable, but safe to source too (the image sources init files that are not
# executable): it never calls exit, and errexit, its only shell option, is already on
# in the image's entrypoint. Any failing command aborts the initialisation.
set -e

: "${POSTGRES_USER:?}" "${POSTGRES_DB:?}"
: "${DB_OWNER:?DB_OWNER is not set}" "${DB_OWNER_PASSWORD_FILE:?DB_OWNER_PASSWORD_FILE is not set}"

case "$DB_OWNER" in
  "$POSTGRES_USER" | pg_* | sc_*)
    echo "10-schema-owner: DB_OWNER cannot be '$DB_OWNER' (the superuser, a reserved name or an application role)" >&2
    false
    ;;
esac
test -s "$DB_OWNER_PASSWORD_FILE" || {
  echo "10-schema-owner: $DB_OWNER_PASSWORD_FILE is missing or empty" >&2
  false
}

psql -v ON_ERROR_STOP=1 --no-psqlrc --username "$POSTGRES_USER" --dbname postgres \
  --set owner="$DB_OWNER" --set db="$POSTGRES_DB" <<'SQL'
CREATE ROLE :"owner" LOGIN CREATEROLE NOSUPERUSER NOCREATEDB NOREPLICATION NOBYPASSRLS;
ALTER DATABASE :"db" OWNER TO :"owner";
SQL

# The same line twice: \password asks for the new password and its confirmation.
{
  cat "$DB_OWNER_PASSWORD_FILE"; echo
  cat "$DB_OWNER_PASSWORD_FILE"; echo
} | psql -v ON_ERROR_STOP=1 --no-psqlrc --quiet --username "$POSTGRES_USER" --dbname postgres \
      -c "\\password $DB_OWNER"

echo "10-schema-owner: role $DB_OWNER created (LOGIN CREATEROLE, not a superuser), owner of database $POSTGRES_DB"
