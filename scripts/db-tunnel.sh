#!/usr/bin/env bash
# DBA access to the production RDS instance on AWS: a Session Manager port forward through
# the DBA bastion (terraform/bastion.tf). Nothing listens on the internet: the bastion has
# no inbound rule, and the forward can only end at the database. DEPLOYMENT.md, "Initial
# DBA Bootstrap & Admin Creation", is the runbook.
#
#   scripts/db-tunnel.sh [--outputs FILE] [--tf-dir DIR] [--local-port PORT]
#                        [--ca-bundle FILE] [--psql [-- PSQL_ARGS...]]
#
#   --outputs FILE    the dba_tunnel Terraform output saved as JSON, for a DBA without
#                     access to the state:
#                       terraform output -json dba_tunnel > dba-tunnel.json
#                     (the whole `terraform output -json` works too). Without it the output
#                     is read live with `terraform -chdir=<dir> output -json dba_tunnel`,
#                     which needs an initialised directory and read access to the state.
#   --tf-dir DIR      Terraform directory for the live read (default: <repo>/terraform)
#   --local-port N    local end of the tunnel, on 127.0.0.1 only (default 5432)
#   --ca-bundle FILE  RDS CA bundle that psql verifies the server with (default: downloaded
#                     once from the output's rds_ca_bundle_url to ~/.cache/synthetic-capital)
#   --psql            open the tunnel, run psql through it as the master user, and close the
#                     tunnel when psql exits. The password goes from Secrets Manager into a
#                     private temporary file, never to the screen, the history or a command
#                     line. Arguments after -- go to psql, e.g. -- -v ON_ERROR_STOP=1 -f x.sql
#
# Without --psql the tunnel stays in the foreground until Ctrl-C, after printing how to
# connect with psql or pgAdmin.
#
# Needs: AWS CLI v2 with credentials (AWS_PROFILE, IAM Identity Center, ...) that hold the
# dba-tunnel policy (terraform output dba_tunnel_policy_arn), the Session Manager plugin,
# jq, curl (unless --ca-bundle), and psql for --psql. TLS is verify-full end to end: psql
# connects to 127.0.0.1 (hostaddr) and checks the certificate against the RDS host name.
# Runs with the bash 3.2 that macOS ships.
set -euo pipefail

die() { echo "db-tunnel: $*" >&2; exit 1; }
info() { echo "db-tunnel: $*" >&2; }
need() { command -v "$1" >/dev/null 2>&1 || die "$1 not found: $2"; }

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
outputs_file=""
tf_dir="$repo_root/terraform"
local_port=5432
ca_bundle=""
run_psql=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --outputs) [[ $# -ge 2 ]] || die "--outputs needs a file"; outputs_file="$2"; shift 2 ;;
    --tf-dir) [[ $# -ge 2 ]] || die "--tf-dir needs a directory"; tf_dir="$2"; shift 2 ;;
    --local-port) [[ $# -ge 2 ]] || die "--local-port needs a port"; local_port="$2"; shift 2 ;;
    --ca-bundle) [[ $# -ge 2 ]] || die "--ca-bundle needs a file"; ca_bundle="$2"; shift 2 ;;
    --psql) run_psql=1; shift ;;
    --) shift; break ;;
    -h|--help) awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"; exit 0 ;;
    *) die "unknown argument: $1 (see --help)" ;;
  esac
done
if [[ $# -gt 0 && $run_psql -eq 0 ]]; then
  die "arguments after -- are for psql: add --psql"
fi
if ! [[ "$local_port" =~ ^[0-9]{1,5}$ ]] || [[ "$local_port" -lt 1 || "$local_port" -gt 65535 ]]; then
  die "--local-port must be a port number (1-65535)"
fi

need aws "install the AWS CLI v2"
need session-manager-plugin "install the Session Manager plugin for the AWS CLI"
need jq "install jq"
if [[ $run_psql -eq 1 ]]; then
  need psql "install the PostgreSQL client (psql)"
fi

# --- What to connect to: the dba_tunnel Terraform output --------------------------------
if [[ -n "$outputs_file" ]]; then
  [[ -r "$outputs_file" ]] || die "cannot read $outputs_file"
  raw="$(cat "$outputs_file")"
else
  need terraform "install Terraform, or pass --outputs FILE"
  raw="$(terraform -chdir="$tf_dir" output -json dba_tunnel)" \
    || die "terraform output failed in $tf_dir (initialised, with read access to the state?); without state access, use --outputs FILE"
fi
# The dba_tunnel object itself, or the whole `terraform output -json`.
tunnel="$(jq -c 'if type == "object" and has("dba_tunnel") then .dba_tunnel.value else . end' <<<"$raw")" \
  || die "the Terraform outputs are not JSON"
if [[ "$tunnel" == "null" ]]; then
  die "the DBA bastion is disabled (dba_bastion_enabled = false); enable it with a reviewed terraform apply"
fi

field() {
  jq -er --arg k "$1" '.[$k] // empty | tostring' <<<"$tunnel" \
    || die "the dba_tunnel output has no $1 (saved from an older configuration?)"
}
region="$(field region)"
instance_id="$(field bastion_instance_id)"
document="$(field session_document)"
db_host="$(field db_host)"
db_port="$(field db_port)"
db_name="$(field db_name)"
db_user="$(field db_user)"
secret_arn="$(field db_secret_arn)"
ca_url="$(field rds_ca_bundle_url)"

# These values end up in a connection string and in AWS CLI arguments: check their shape.
[[ "$region" =~ ^[a-z]{2}(-[a-z]+)+-[0-9]+$ ]] || die "unexpected region: $region"
[[ "$instance_id" =~ ^i-[0-9a-f]{8,17}$ ]] || die "unexpected bastion_instance_id: $instance_id"
[[ "$db_host" =~ ^[A-Za-z0-9.-]+$ ]] || die "unexpected db_host: $db_host"
[[ "$db_port" =~ ^[0-9]{1,5}$ ]] || die "unexpected db_port: $db_port"
[[ "$db_name" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || die "unexpected db_name: $db_name"
[[ "$db_user" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || die "unexpected db_user: $db_user"
[[ "$ca_url" == https://* ]] || die "unexpected rds_ca_bundle_url: $ca_url"

# --- RDS CA bundle (not secret; cached so pgAdmin can point at it too) -------------------
if [[ -z "$ca_bundle" ]]; then
  need curl "install curl, or pass --ca-bundle FILE"
  cache="${XDG_CACHE_HOME:-$HOME/.cache}/synthetic-capital"
  ca_bundle="$cache/rds-ca-$region.pem"
  if [[ ! -s "$ca_bundle" ]]; then
    mkdir -p "$cache"
    download="$(mktemp "$cache/.rds-ca.XXXXXX")"
    if ! curl --fail --silent --show-error --proto '=https' --tlsv1.2 --max-time 30 -o "$download" "$ca_url"; then
      rm -f "$download"
      die "could not download $ca_url (or pass --ca-bundle FILE)"
    fi
    if ! grep -q -- '-----BEGIN CERTIFICATE-----' "$download"; then
      rm -f "$download"
      die "$ca_url is not a PEM bundle"
    fi
    mv "$download" "$ca_bundle"
  fi
fi
[[ -r "$ca_bundle" ]] || die "cannot read $ca_bundle"

# --- Pre-flight ---------------------------------------------------------------------------
ping="$(aws ssm describe-instance-information --region "$region" \
  --filters "Key=InstanceIds,Values=$instance_id" \
  --query 'InstanceInformationList[0].PingStatus' --output text)" \
  || die "cannot query Systems Manager (expired credentials? the dba-tunnel policy?)"
case "$ping" in
  Online) ;;
  None|"") die "the bastion $instance_id is not registered with Systems Manager: replaced since the outputs were saved (refresh them), or still booting (wait a minute)" ;;
  *) die "the bastion $instance_id is '$ping' in Systems Manager (stopped, or its agent lost contact)" ;;
esac

if (exec 3<>"/dev/tcp/127.0.0.1/$local_port") 2>/dev/null; then
  die "127.0.0.1:$local_port is already in use (a local PostgreSQL?): pass --local-port, e.g. 15432"
fi

params="$(jq -cn --arg h "$db_host" --arg p "$db_port" --arg l "$local_port" \
  '{host: [$h], portNumber: [$p], localPortNumber: [$l]}')"
tunnel_cmd=(aws ssm start-session --region "$region" --target "$instance_id"
  --document-name "$document" --parameters "$params")

# A libpq connection-string value, quoted.
conn_value() {
  local v="${1//\\/\\\\}"
  v="${v//\'/\\\'}"
  printf "'%s'" "$v"
}
conninfo="host=$db_host hostaddr=127.0.0.1 port=$local_port dbname=$db_name user=$db_user"
conninfo="$conninfo sslmode=verify-full sslrootcert=$(conn_value "$ca_bundle") application_name=db-tunnel"

# --- Tunnel only: the DBA connects with psql or pgAdmin ----------------------------------
if [[ $run_psql -eq 0 ]]; then
  cat >&2 <<EOF
db-tunnel: 127.0.0.1:$local_port -> $db_host:$db_port through $instance_id ($region)

  psql:     psql "$conninfo"
  password: aws secretsmanager get-secret-value --region $region --secret-id '$secret_arn' \\
              --query SecretString --output text | jq -r .password
            (RDS rotates it; never save it)
  pgAdmin:  General > Name: anything. Connection > Host name/address: $db_host,
            Port: $local_port, Maintenance database: $db_name, Username: $db_user,
            Save password: off. Parameters > Host address: 127.0.0.1,
            SSL mode: verify-full, Root certificate: $ca_bundle

  Ctrl-C closes the tunnel. Or run scripts/db-tunnel.sh --psql to do it all in one step.

EOF
  exec "${tunnel_cmd[@]}"
fi

# --- --psql: tunnel in the background, psql in the foreground ----------------------------
umask 077
workdir="$(mktemp -d "${TMPDIR:-/tmp}/db-tunnel.XXXXXX")"
tunnel_pid=""
# shellcheck disable=SC2329 # invoked by the EXIT trap below
cleanup() {
  if [[ -n "$tunnel_pid" ]]; then
    # Ctrl-C for the plugin: it ends the Session Manager session cleanly. The tunnel leads
    # its own process group (set -m below); pkill -P covers the plugin otherwise.
    kill -INT -- "-$tunnel_pid" 2>/dev/null || pkill -INT -P "$tunnel_pid" 2>/dev/null || true
    local i=0
    while kill -0 "$tunnel_pid" 2>/dev/null && [[ $i -lt 20 ]]; do
      sleep 0.25
      i=$((i + 1))
    done
    if kill -0 "$tunnel_pid" 2>/dev/null; then
      kill -TERM -- "-$tunnel_pid" 2>/dev/null || pkill -TERM -P "$tunnel_pid" 2>/dev/null || true
      kill -TERM "$tunnel_pid" 2>/dev/null || true
    fi
    wait "$tunnel_pid" 2>/dev/null || true
  fi
  rm -rf "$workdir"
}
trap cleanup EXIT
trap 'exit 130' INT TERM HUP

# Job control puts the tunnel in its own process group, so a Ctrl-C that cancels a query
# in psql does not reach the tunnel.
set -m
"${tunnel_cmd[@]}" </dev/null >"$workdir/tunnel.log" 2>&1 &
tunnel_pid=$!

info "opening the tunnel through $instance_id ..."
i=0
until grep -q 'Waiting for connections' "$workdir/tunnel.log" 2>/dev/null; do
  if ! kill -0 "$tunnel_pid" 2>/dev/null; then
    cat "$workdir/tunnel.log" >&2
    die "the tunnel did not start (output above)"
  fi
  if [[ $i -ge 120 ]]; then
    cat "$workdir/tunnel.log" >&2
    die "the tunnel did not open within 30 seconds"
  fi
  sleep 0.25
  i=$((i + 1))
done

# pgpass format: backslash and colon escaped. The password never leaves the pipe and the file.
if ! aws secretsmanager get-secret-value --region "$region" --secret-id "$secret_arn" \
    --query SecretString --output text \
  | jq -er '"*:*:*:*:" + (.password | gsub("\\\\"; "\\\\") | gsub(":"; "\\:"))' >"$workdir/pgpass"; then
  die "cannot read the master user secret $secret_arn (the dba-tunnel policy?)"
fi

info "connected through 127.0.0.1:$local_port as $db_user (verify-full); the tunnel closes when psql exits"
set +e
PGPASSFILE="$workdir/pgpass" psql -X -d "$conninfo" "$@"
status=$?
set -e
exit "$status"
