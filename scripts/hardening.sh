#!/usr/bin/env bash
# Host hardening for the single-host docker compose deployment of Synthetic Capital.
# Supported: Ubuntu 22.04 / 24.04 and Debian 12. Idempotent: every run converges the
# host to the same state and reports what changed; a second run changes nothing.
#
#   sudo scripts/hardening.sh --admin-user USER[,USER...] --ssh-allow CIDR[,CIDR...] [options]
#
#   --admin-user USERS     accounts allowed to log in over SSH (AllowUsers). Each must exist,
#                          have a usable key in ~/.ssh/authorized_keys and be able to sudo.
#   --ssh-allow CIDRS      source networks allowed to reach SSH, e.g. 203.0.113.0/24,2001:db8::/48
#                          (required: SSH is never opened to the world by default)
#   --ssh-port PORT        SSH port (default 22)
#   --deploy-dir DIR       docker compose deployment directory (default /opt/synthetic-capital)
#   --nginx-uid UID        uid (= gid) the nginx container runs as (default 101, the image's
#                          nginx user); owns logs/nginx and certs/privkey.pem on the host
#   --allow-password-auth  keep SSH password logins (only to bootstrap keys; not recommended)
#   --skip-docker          leave /etc/docker/daemon.json alone
#   --dry-run              show what would change (with diffs) and change nothing
#   --yes                  apply without asking for confirmation
#   -h, --help             this help
#
# Steps: packages; unattended security upgrades; UFW (deny incoming, rate-limited SSH from
# the allowed networks only, 80/443 open); an sshd drop-in (no root login, keys only,
# AllowUsers, modern KEX/ciphers/MACs, no forwarding) validated with `sshd -t` before any
# reload; deployment directory permissions; fail2ban (sshd jail, plus nginx jails that read
# the edge's mounted logs and ban in Docker's DOCKER-USER chain); sysctl network hardening;
# /etc/docker/daemon.json defaults; chrony; auditd baseline rules; logrotate for the nginx
# logs. Lockout guards run before anything changes. Without systemd (e.g. in a container)
# it writes and validates everything it can and reports what it skipped.
set -Eeuo pipefail
umask 022
export LC_ALL=C

readonly SCRIPT_NAME="sc-hardening"
readonly TAG="sc-hardening"            # comment on the UFW rules this script owns
readonly ORIG_ARGS=("$@")

# ------------------------------------------------------------------------------------------
# Arguments
# ------------------------------------------------------------------------------------------
DRY_RUN=0
ASSUME_YES=0
ALLOW_PASSWORD=0
SKIP_DOCKER=0
SSH_PORT=22
DEPLOY_DIR=/opt/synthetic-capital
NGINX_UID=101          # nginx user of the official nginx image (see --nginx-uid)
ADMIN_USERS=()
SSH_ALLOW=()

usage() { awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"; }
die() { echo "$SCRIPT_NAME: error: $*" >&2; exit 1; }

split_csv() { # split_csv "a,b, c": one item per line, blanks dropped
  local IFS=',' item
  for item in $1; do
    item="${item//[[:space:]]/}"
    [[ -n "$item" ]] && printf '%s\n' "$item"
  done
  return 0
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --admin-user) [[ $# -ge 2 ]] || die "--admin-user needs a value"; mapfile -t -O "${#ADMIN_USERS[@]}" ADMIN_USERS < <(split_csv "$2"); shift 2 ;;
    --ssh-allow) [[ $# -ge 2 ]] || die "--ssh-allow needs a value"; mapfile -t -O "${#SSH_ALLOW[@]}" SSH_ALLOW < <(split_csv "$2"); shift 2 ;;
    --ssh-port) [[ $# -ge 2 ]] || die "--ssh-port needs a value"; SSH_PORT="$2"; shift 2 ;;
    --deploy-dir) [[ $# -ge 2 ]] || die "--deploy-dir needs a value"; DEPLOY_DIR="${2%/}"; shift 2 ;;
    --nginx-uid) [[ $# -ge 2 ]] || die "--nginx-uid needs a value"; NGINX_UID="$2"; shift 2 ;;
    --allow-password-auth) ALLOW_PASSWORD=1; shift ;;
    --skip-docker) SKIP_DOCKER=1; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --yes|-y) ASSUME_YES=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown argument: $1" ;;
  esac
done

# ------------------------------------------------------------------------------------------
# Output, results and helpers
# ------------------------------------------------------------------------------------------
RESULT_STEP=()
RESULT_STATUS=()
RESULT_DETAIL=()
WARNINGS=()
NEXT_STEPS=()
FAILED=0
RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)"
BACKUP_DIR="/var/backups/$SCRIPT_NAME/$RUN_ID"

say()  { printf '%s\n' "$*"; }
info() { printf '  - %s\n' "$*"; }
warn() { printf '  ! %s\n' "$*" >&2; WARNINGS+=("$*"); }
step() { printf '\n== %s\n' "$*"; }
# record <step> <changed|unchanged|would-change|skipped|partial|failed> <detail>
record() {
  RESULT_STEP+=("$1"); RESULT_STATUS+=("$2"); RESULT_DETAIL+=("$3")
  [[ "$2" == failed ]] && FAILED=1
  return 0
}
# outcome <changed-flag>: the status word for a step that did (or would do) something
outcome() { if [[ "$1" -eq 0 ]]; then echo unchanged; elif [[ $DRY_RUN -eq 1 ]]; then echo would-change; else echo changed; fi; }
in_list() { # in_list <needle> <items...>
  local needle="$1" item; shift
  for item in "$@"; do [[ "$item" == "$needle" ]] && return 0; done
  return 1
}
have() { command -v "$1" >/dev/null 2>&1; }

HAVE_SYSTEMD=0
if [[ -d /run/systemd/system ]] && have systemctl; then HAVE_SYSTEMD=1; fi
svc_active() { [[ $HAVE_SYSTEMD -eq 1 ]] && systemctl is-active --quiet "$1"; }

# CHANGED=1 after apply_file / ensure_dir / ensure_mode changed (or in dry-run: would
# change) something. These helpers must run in the current shell, never in a pipeline.
CHANGED=0

backup() { # backup <path>: copy into this run's backup directory (once per path)
  local src="$1" dst
  [[ -e "$src" ]] || return 0
  dst="$BACKUP_DIR$src"
  [[ -e "$dst" ]] && return 0
  mkdir -p "$(dirname "$dst")"
  cp -a "$src" "$dst"
}

owner_ids() { # owner_ids user:group -> uid:gid (numeric input passes through)
  local u="${1%%:*}" g="${1##*:}"
  [[ "$u" =~ ^[0-9]+$ ]] || u="$(id -u "$u")"
  [[ "$g" =~ ^[0-9]+$ ]] || g="$(getent group "$g" | cut -d: -f3)"
  printf '%s:%s' "$u" "$g"
}

# apply_file <dest> <mode> <owner:group>   (desired content on stdin)
apply_file() {
  local dest="$1" mode="$2" owner="$3" tmp
  tmp="$(mktemp)"
  cat > "$tmp"
  CHANGED=0
  if [[ -f "$dest" ]] && cmp -s "$tmp" "$dest" \
     && [[ "$(stat -c '%a' "$dest")" == "${mode#0}" && "$(stat -c '%u:%g' "$dest")" == "$(owner_ids "$owner")" ]]; then
    rm -f "$tmp"; return 0
  fi
  CHANGED=1
  if [[ $DRY_RUN -eq 1 ]]; then
    if [[ -f "$dest" ]]; then
      info "would update $dest:"
      diff -u --label "$dest (current)" --label "$dest (new)" "$dest" "$tmp" | sed 's/^/      /' || true
    else
      info "would create $dest (mode $mode, owner $owner):"
      sed 's/^/      | /' "$tmp"
    fi
    rm -f "$tmp"; return 0
  fi
  backup "$dest"
  mkdir -p "$(dirname "$dest")"
  chmod "$mode" "$tmp"
  chown "$owner" "$tmp"
  mv -f "$tmp" "$dest"
  info "wrote $dest"
}

# ensure_dir <dir> <mode> <owner:group>
ensure_dir() {
  local dir="$1" mode="$2" owner="$3"
  CHANGED=0
  if [[ -d "$dir" ]]; then
    [[ "$(stat -c '%a' "$dir")" == "${mode#0}" && "$(stat -c '%u:%g' "$dir")" == "$(owner_ids "$owner")" ]] && return 0
    CHANGED=1
    if [[ $DRY_RUN -eq 1 ]]; then info "would set $dir to mode $mode, owner $owner (now $(stat -c '%a %U:%G' "$dir"))"; return 0; fi
    chmod "$mode" "$dir"; chown "$owner" "$dir"
    info "set $dir to mode $mode, owner $owner"
    return 0
  fi
  CHANGED=1
  if [[ $DRY_RUN -eq 1 ]]; then info "would create $dir (mode $mode, owner $owner)"; return 0; fi
  mkdir -p "$dir"; chmod "$mode" "$dir"; chown "$owner" "$dir"
  info "created $dir (mode $mode, owner $owner)"
}

# ensure_mode <path> <mode> [owner:group]: only for paths that exist; contents untouched
ensure_mode() {
  local path="$1" mode="$2" owner="${3:-}"
  CHANGED=0
  [[ -e "$path" ]] || return 0
  if [[ "$(stat -c '%a' "$path")" == "${mode#0}" ]] && { [[ -z "$owner" ]] || [[ "$(stat -c '%u:%g' "$path")" == "$(owner_ids "$owner")" ]]; }; then
    return 0
  fi
  CHANGED=1
  if [[ $DRY_RUN -eq 1 ]]; then info "would set $path to mode $mode${owner:+, owner $owner} (now $(stat -c '%a %U:%G' "$path"))"; return 0; fi
  chmod "$mode" "$path"
  if [[ -n "$owner" ]]; then chown "$owner" "$path"; fi
  info "set $path to mode $mode${owner:+, owner $owner}"
}

# ------------------------------------------------------------------------------------------
# Preflight: environment, parameters and lockout guards. Nothing changes before they pass.
# ------------------------------------------------------------------------------------------
step "Preflight"
[[ ${#ADMIN_USERS[@]} -gt 0 ]] || { usage >&2; die "--admin-user is required"; }
[[ ${#SSH_ALLOW[@]} -gt 0 ]] || { usage >&2; die "--ssh-allow is required (SSH is never opened to everyone by default)"; }
{ [[ "$SSH_PORT" =~ ^[0-9]{1,5}$ ]] && (( SSH_PORT >= 1 && SSH_PORT <= 65535 )); } || die "--ssh-port must be 1-65535"
[[ "$DEPLOY_DIR" == /?* ]] || die "--deploy-dir must be an absolute path other than /"
{ [[ "$NGINX_UID" =~ ^[0-9]{1,7}$ ]] && (( NGINX_UID > 0 )); } || die "--nginx-uid must be a positive number"
NGINX_GID="$NGINX_UID"
if [[ $EUID -ne 0 ]]; then
  [[ $DRY_RUN -eq 1 ]] || die "run as root (sudo), or with --dry-run"
  warn "not root: the dry-run cannot read every file, results are partial"
fi

[[ -r /etc/os-release ]] || die "/etc/os-release not found"
# shellcheck source=/dev/null
OS_ID="$(. /etc/os-release && echo "${ID:-}")"
# shellcheck source=/dev/null
OS_VERSION="$(. /etc/os-release && echo "${VERSION_ID:-}")"
# shellcheck source=/dev/null
OS_NAME="$(. /etc/os-release && echo "${PRETTY_NAME:-$ID}")"
case "$OS_ID:$OS_VERSION" in
  ubuntu:22.04|ubuntu:24.04|debian:12) ;;
  *) die "unsupported system: $OS_NAME (supported: Ubuntu 22.04, Ubuntu 24.04, Debian 12)" ;;
esac
info "system: $OS_NAME; systemd: $([[ $HAVE_SYSTEMD -eq 1 ]] && echo yes || echo 'no (services are not started; reported as skipped)')"

valid_cidr() {
  local c="$1" addr prefix o
  [[ "$c" == */* ]] || return 1
  addr="${c%/*}"; prefix="${c##*/}"
  [[ "$prefix" =~ ^[0-9]{1,3}$ ]] || return 1
  if [[ "$addr" =~ ^([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})$ ]]; then
    for o in "${BASH_REMATCH[@]:1}"; do (( 10#$o <= 255 )) || return 1; done
    (( 10#$prefix <= 32 ))
  elif [[ "$addr" == *:* && "$addr" =~ ^[0-9A-Fa-f:]+$ ]]; then
    (( 10#$prefix <= 128 ))
  else
    return 1
  fi
}
for c in "${SSH_ALLOW[@]}"; do valid_cidr "$c" || die "invalid CIDR in --ssh-allow: $c (expected e.g. 203.0.113.0/24)"; done

ipv4_to_int() { local IFS=. a b c d; read -r a b c d <<<"$1"; echo $(( (10#$a << 24) | (10#$b << 16) | (10#$c << 8) | 10#$d )); }
ip_in_cidrs() { # ip_in_cidrs <ip>: 0 inside --ssh-allow, 1 outside, 2 cannot tell
  local ip="$1" c net prefix mask
  if [[ "$ip" == *:* ]]; then
    have python3 || return 2
    python3 - "$ip" "${SSH_ALLOW[@]}" <<'PY'
import ipaddress, sys
ip = ipaddress.ip_address(sys.argv[1])
sys.exit(0 if any(ip in ipaddress.ip_network(c, strict=False) for c in sys.argv[2:] if ":" in c) else 1)
PY
    return $?
  fi
  [[ "$ip" =~ ^[0-9]{1,3}(\.[0-9]{1,3}){3}$ ]] || return 2
  for c in "${SSH_ALLOW[@]}"; do
    [[ "$c" == *:* ]] && continue
    net="${c%/*}"; prefix="${c##*/}"
    (( prefix == 0 )) && return 0
    mask=$(( (0xFFFFFFFF << (32 - prefix)) & 0xFFFFFFFF ))
    (( ($(ipv4_to_int "$ip") & mask) == ($(ipv4_to_int "$net") & mask) )) && return 0
  done
  return 1
}

# Guard 1: the SSH session running this script stays able to reconnect.
if [[ -n "${SSH_CONNECTION:-}" ]]; then
  client_ip="${SSH_CONNECTION%% *}"; client_ip="${client_ip#::ffff:}"
  rc=0; ip_in_cidrs "$client_ip" || rc=$?
  case $rc in
    0) info "this SSH session's address $client_ip is inside --ssh-allow" ;;
    1) die "lockout guard: this SSH session comes from $client_ip, outside --ssh-allow (${SSH_ALLOW[*]})" ;;
    *) warn "cannot verify that $client_ip is inside --ssh-allow; make sure it is" ;;
  esac
  invoking="${SUDO_USER:-$(id -un)}"
  if [[ "$invoking" != root ]] && ! in_list "$invoking" "${ADMIN_USERS[@]}"; then
    die "lockout guard: you are logged in as '$invoking', which AllowUsers would no longer admit; add it to --admin-user"
  fi
fi

# Guard 2: every admin user exists, can sudo (root logins are disabled) and, unless
# passwords stay on, has a key sshd will accept (StrictModes-clean files).
authorized_key_files() { # authorized_key_files <user> <home>
  local user="$1" home="$2" spec="" f
  if have sshd && [[ $EUID -eq 0 ]]; then
    mkdir -p /run/sshd 2>/dev/null || true
    spec="$(sshd -T -C "user=$user,host=localhost,addr=127.0.0.1" 2>/dev/null | awk '$1 == "authorizedkeysfile" { $1 = ""; print }')" || spec=""
  fi
  [[ -n "${spec// /}" ]] || spec=".ssh/authorized_keys .ssh/authorized_keys2"
  for f in $spec; do
    f="${f//%h/$home}"; f="${f//%u/$user}"; f="${f//%%/%}"
    [[ "$f" == /* ]] || f="$home/$f"
    printf '%s\n' "$f"
  done
}
usable_keys() { # usable_keys <file>: keys sshd would accept (ed25519, ecdsa, RSA >= 2048 bits)
  local file="$1" listing
  if have ssh-keygen; then
    listing="$(ssh-keygen -l -f "$file" 2>/dev/null)" || listing=""
    awk '{ t = $NF; b = $1 }
         t ~ /^\((ED25519|ED25519-SK|ECDSA|ECDSA-SK)\)$/ { n++; next }
         t == "(RSA)" && b >= 2048 { n++ }
         END { print n + 0 }' <<<"$listing"
  else
    grep -Ec '(^|[[:space:]])(ssh-ed25519|sk-ssh-ed25519@openssh\.com|ecdsa-sha2-nistp(256|384|521)|sk-ecdsa-sha2-nistp256@openssh\.com) AAAA' "$file" || true
  fi
}
strict_modes_problem() { # strict_modes_problem <user> <path>: prints the problem, if any
  local user="$1" path="$2" owner mode
  owner="$(stat -c '%U' "$path")"; mode="$(stat -c '%a' "$path")"
  if [[ "$owner" != "$user" && "$owner" != root ]]; then echo "$path is owned by $owner"
  elif (( (8#$mode & 8#022) != 0 )); then echo "$path is writable by group/others (mode $mode)"; fi
  return 0
}
for u in "${ADMIN_USERS[@]}"; do
  [[ "$u" =~ ^[a-z_][a-z0-9_.-]*[$]?$ ]] || die "invalid user name: $u"
  [[ "$u" != root ]] || die "root cannot be an admin user: root logins are disabled"
  pw="$(getent passwd "$u")" || die "admin user '$u' does not exist (create it first)"
  home="$(cut -d: -f6 <<<"$pw")"
  groups_of=" $(id -nG "$u") "
  sudo_rules=""
  if have sudo; then sudo_rules="$(sudo -n -l -U "$u" 2>/dev/null)" || sudo_rules=""; fi
  if [[ "$groups_of" != *" sudo "* && "$groups_of" != *" admin "* && "$groups_of" != *" wheel "* && "$sudo_rules" != *"may run the following"* ]]; then
    die "lockout guard: admin user '$u' cannot use sudo (add it to the sudo group): with root logins disabled nobody could administer the host"
  fi
  keys=0
  while IFS= read -r f; do
    [[ -r "$f" ]] || continue
    for p in "$home" "$(dirname "$f")" "$f"; do
      problem="$(strict_modes_problem "$u" "$p")"
      [[ -z "$problem" ]] || die "lockout guard: sshd (StrictModes) would ignore $u's keys: $problem"
    done
    n="$(usable_keys "$f")"; keys=$(( keys + n ))
  done < <(authorized_key_files "$u" "$home")
  if [[ $keys -gt 0 ]]; then
    info "admin user '$u': $keys usable SSH key(s), can sudo"
  elif [[ $ALLOW_PASSWORD -eq 1 ]]; then
    warn "admin user '$u' has no usable SSH key; password logins stay enabled (--allow-password-auth)"
  else
    die "lockout guard: admin user '$u' has no usable key in authorized_keys (ed25519, ecdsa or RSA >= 2048), so password logins will not be disabled. Install a key first (ssh-copy-id), or pass --allow-password-auth."
  fi
done

say ""
say "Plan ($([[ $DRY_RUN -eq 1 ]] && echo 'dry-run: nothing will be changed' || echo 'apply')):"
info "SSH: port $SSH_PORT from ${SSH_ALLOW[*]}; users ${ADMIN_USERS[*]}; passwords $([[ $ALLOW_PASSWORD -eq 1 ]] && echo KEPT || echo off); root login off"
info "firewall: UFW deny incoming; 80/443 open; SSH rate-limited from the allowed networks"
info "deployment: $DEPLOY_DIR (secrets/ 0700, deploy/compose.env 0640, logs/nginx for uid $NGINX_UID)"
info "fail2ban: sshd + nginx jails on $DEPLOY_DIR/logs/nginx, web bans in DOCKER-USER"
info "also: unattended-upgrades, sysctl, docker daemon defaults$([[ $SKIP_DOCKER -eq 1 ]] && echo ' (skipped)'), chrony, auditd, logrotate"
if [[ $DRY_RUN -eq 0 && $ASSUME_YES -eq 0 ]]; then
  [[ -t 0 ]] || die "no terminal to confirm on: re-run with --yes (or try --dry-run first)"
  read -r -p "Apply these changes? [y/N] " answer || answer=""
  [[ "$answer" =~ ^[Yy]([Ee][Ss])?$ ]] || die "aborted, nothing changed"
fi
if [[ $DRY_RUN -eq 0 ]]; then
  exec > >(tee -a "/var/log/$SCRIPT_NAME.log") 2>&1
  say "--- $SCRIPT_NAME $RUN_ID: ${ORIG_ARGS[*]}"
fi

# ------------------------------------------------------------------------------------------
# 1. Packages
# ------------------------------------------------------------------------------------------
step "Packages"
PACKAGES=(unattended-upgrades ufw fail2ban python3 python3-systemd chrony auditd openssh-server openssh-client sudo logrotate iptables)
missing=()
for p in "${PACKAGES[@]}"; do
  [[ "$(dpkg-query -W -f='${Status}' "$p" 2>/dev/null || true)" == "install ok installed" ]] || missing+=("$p")
done
if [[ ${#missing[@]} -eq 0 ]]; then
  record packages unchanged "all present"
elif [[ $DRY_RUN -eq 1 ]]; then
  info "would install: ${missing[*]}"
  record packages would-change "would install: ${missing[*]}"
else
  # No prompts, and no service restarts by needrestart halfway through the run (it could
  # restart dockerd, and with it every container).
  export DEBIAN_FRONTEND=noninteractive NEEDRESTART_SUSPEND=1
  if apt-get update -q && apt-get install -y -q --no-install-recommends "${missing[@]}"; then
    record packages changed "installed: ${missing[*]}"
  else
    record packages failed "apt-get could not install: ${missing[*]}"
    die "package installation failed; nothing else was changed"
  fi
fi

# ------------------------------------------------------------------------------------------
# 2. Unattended security upgrades
# ------------------------------------------------------------------------------------------
step "Unattended upgrades"
uu_changed=0
apply_file /etc/apt/apt.conf.d/20auto-upgrades 0644 root:root <<'EOF'
// Managed by scripts/hardening.sh (Synthetic Capital).
APT::Periodic::Update-Package-Lists "1";
APT::Periodic::Download-Upgradeable-Packages "1";
APT::Periodic::Unattended-Upgrade "1";
APT::Periodic::AutocleanInterval "7";
EOF
uu_changed=$(( uu_changed | CHANGED ))
# The distribution's 50unattended-upgrades already selects the security pocket; this file
# adds policy only. Reboots stay manual (maintenance window; /var/run/reboot-required), and
# Docker CE from download.docker.com is not auto-upgraded (an upgrade restarts dockerd).
apply_file /etc/apt/apt.conf.d/52sc-hardening 0644 root:root <<'EOF'
// Managed by scripts/hardening.sh (Synthetic Capital).
Unattended-Upgrade::Remove-Unused-Kernel-Packages "true";
Unattended-Upgrade::Remove-New-Unused-Dependencies "true";
Unattended-Upgrade::Remove-Unused-Dependencies "true";
Unattended-Upgrade::Automatic-Reboot "false";
Unattended-Upgrade::SyslogEnable "true";
EOF
uu_changed=$(( uu_changed | CHANGED ))
if have apt-config && ! apt-config dump >/dev/null 2>&1; then
  record unattended-upgrades failed "apt-config cannot parse the configuration"
else
  if [[ $HAVE_SYSTEMD -eq 1 ]] && ! systemctl is-enabled --quiet apt-daily-upgrade.timer 2>/dev/null; then
    if [[ $DRY_RUN -eq 1 ]]; then info "would enable apt-daily.timer and apt-daily-upgrade.timer"; uu_changed=1
    else systemctl enable --now apt-daily.timer apt-daily-upgrade.timer >/dev/null 2>&1 || warn "could not enable the apt timers"; uu_changed=1; fi
  fi
  record unattended-upgrades "$(outcome $uu_changed)" "security updates daily; no automatic reboot"
fi
[[ -f /var/run/reboot-required ]] && NEXT_STEPS+=("A reboot is pending (/var/run/reboot-required): schedule one.")

# ------------------------------------------------------------------------------------------
# 3. Firewall (UFW)
# ------------------------------------------------------------------------------------------
# Docker publishes ports with DNAT (nat table), so traffic to published container ports is
# forwarded and never reaches UFW's INPUT rules: UFW guards the host's own services (SSH).
# 80/443 are opened to state intent (and for an nginx on the host network); the compose
# file must publish nothing but nginx's 80/443. fail2ban bans web clients in DOCKER-USER.
step "Firewall (UFW)"
# UFW_SSH_OK=0 means the SSH rules could not all be put in place: UFW is then not enabled,
# and the SSH step will not move sshd to another port (either could lock admins out).
UFW_SSH_OK=1
UFW_ACTIVE=0
if ! have ufw; then
  if [[ $DRY_RUN -eq 1 ]]; then
    for c in "${SSH_ALLOW[@]}"; do info "would run: ufw limit proto tcp from $c to any port $SSH_PORT comment $TAG:ssh"; done
    info "would run: ufw allow 80/tcp, 443/tcp; ufw default deny incoming; ufw --force enable"
    record ufw would-change "ufw not installed yet (packages step)"
  else
    UFW_SSH_OK=0
    record ufw failed "ufw not installed"
  fi
else
  ufw_changed=0
  ufw_errors=()
  before="$(ufw show added 2>/dev/null || true)"
  [[ "$(ufw status 2>/dev/null | head -n 1 || true)" == *"Status: active"* ]] && UFW_ACTIVE=1
  if ! grep -q '^DEFAULT_INPUT_POLICY="DROP"' /etc/default/ufw || ! grep -q '^DEFAULT_OUTPUT_POLICY="ACCEPT"' /etc/default/ufw; then
    ufw_changed=1
    if [[ $DRY_RUN -eq 1 ]]; then info "would run: ufw default deny incoming; ufw default allow outgoing"
    else ufw default deny incoming >/dev/null 2>&1 && ufw default allow outgoing >/dev/null 2>&1 || ufw_errors+=("default policies"); fi
  fi
  want=()
  for c in "${SSH_ALLOW[@]}"; do want+=("limit proto tcp from $c to any port $SSH_PORT comment $TAG:ssh"); done
  want+=("allow proto tcp from any to any port 80 comment $TAG:http")
  want+=("allow proto tcp from any to any port 443 comment $TAG:https")
  for r in "${want[@]}"; do
    # shellcheck disable=SC2086  # a rule is a list of words
    out="$(ufw --dry-run $r 2>&1 || true)"
    [[ "$out" == *"Skipping adding existing rule"* ]] && continue
    if [[ "$out" == *"IPv6 support not enabled"* ]]; then
      warn "IPv6 is not enabled on this host (or in UFW): skipped 'ufw $r'"
      continue
    fi
    ufw_changed=1
    if [[ $DRY_RUN -eq 1 ]]; then info "would run: ufw $r"; continue; fi
    # shellcheck disable=SC2086
    if err="$(ufw $r 2>&1 >/dev/null)"; then
      info "added: ufw $r"
    else
      ufw_errors+=("ufw $r: $(tail -n 1 <<<"$err")")
      [[ "$r" == *"comment $TAG:ssh" ]] && UFW_SSH_OK=0
    fi
  done
  # SSH rules this script added earlier for sources or ports that are no longer wanted
  # (only once the wanted ones are in place).
  while IFS= read -r line; do
    [[ $UFW_SSH_OK -eq 1 ]] || break
    [[ "$line" == "ufw "* && "$line" == *"comment '$TAG:ssh'"* ]] || continue
    src="$(sed -nE 's/.* from ([^ ]+) to any port ([0-9]+).*/\1/p' <<<"$line")"
    port="$(sed -nE 's/.* from ([^ ]+) to any port ([0-9]+).*/\2/p' <<<"$line")"
    if [[ "$port" == "$SSH_PORT" ]] && in_list "$src" "${SSH_ALLOW[@]}"; then continue; fi
    spec="${line#ufw }"; spec="${spec% comment *}"
    ufw_changed=1
    if [[ $DRY_RUN -eq 1 ]]; then info "would remove stale rule: ufw delete $spec"; continue; fi
    # shellcheck disable=SC2086
    if err="$(ufw delete $spec 2>&1 >/dev/null)"; then info "removed stale rule: $spec"; else ufw_errors+=("ufw delete $spec: $(tail -n 1 <<<"$err")"); fi
  done <<<"$before"
  detail="deny incoming; SSH $SSH_PORT/tcp rate-limited from ${SSH_ALLOW[*]}; 80,443/tcp open"
  if [[ ${#ufw_errors[@]} -gt 0 ]]; then
    record ufw failed "${ufw_errors[*]}$([[ $UFW_ACTIVE -eq 0 ]] && echo '; UFW left disabled')"
  elif [[ $UFW_ACTIVE -eq 1 ]]; then
    record ufw "$(outcome $ufw_changed)" "$detail; active"
  elif [[ $DRY_RUN -eq 1 ]]; then
    info "would run: ufw --force enable"
    record ufw would-change "$detail; would enable UFW"
  elif err="$(ufw --force enable 2>&1 >/dev/null)"; then
    UFW_ACTIVE=1
    record ufw changed "$detail; enabled"
  else
    record ufw skipped "rules saved, but UFW could not be enabled here ($(tail -n 1 <<<"$err")); run 'ufw --force enable' on the host"
    NEXT_STEPS+=("Enable the firewall: sudo ufw --force enable (the rules are already in place).")
  fi
fi

# ------------------------------------------------------------------------------------------
# 4. SSH daemon
# ------------------------------------------------------------------------------------------
step "SSH daemon"
SSHD_DROPIN=/etc/ssh/sshd_config.d/01-sc-hardening.conf
pick_algorithms() { # pick_algorithms <ssh -Q type> <preferred...>: supported ones, in order
  local type="$1" supported="" picked=() a; shift
  if have ssh; then supported="$(ssh -Q "$type" 2>/dev/null || true)"; fi
  for a in "$@"; do
    if [[ -z "$supported" ]] || grep -qxF -- "$a" <<<"$supported"; then picked+=("$a"); fi
  done
  local IFS=,; echo "${picked[*]}"
}
current_port=22
if have sshd && [[ $EUID -eq 0 ]]; then
  mkdir -p /run/sshd
  current_port="$(sshd -T 2>/dev/null | awk '$1 == "port" { print $2; exit }' || true)"
  current_port="${current_port:-22}"
fi
if [[ -f /etc/ssh/sshd_config ]] && ! grep -Eq '^[[:space:]]*Include[[:space:]]+/etc/ssh/sshd_config\.d/\*\.conf' /etc/ssh/sshd_config; then
  record sshd failed "/etc/ssh/sshd_config does not include sshd_config.d/*.conf; refusing to guess"
elif [[ "$current_port" != "$SSH_PORT" && $UFW_SSH_OK -eq 0 ]]; then
  record sshd failed "not moving sshd from port $current_port to $SSH_PORT: the firewall rules for the new port could not be added (lockout guard)"
else
  KEX="$(pick_algorithms kex mlkem768x25519-sha256 sntrup761x25519-sha512 sntrup761x25519-sha512@openssh.com curve25519-sha256 curve25519-sha256@libssh.org diffie-hellman-group18-sha512 diffie-hellman-group16-sha512)"
  CIPHERS="$(pick_algorithms cipher chacha20-poly1305@openssh.com aes256-gcm@openssh.com aes128-gcm@openssh.com aes256-ctr aes192-ctr aes128-ctr)"
  MACS="$(pick_algorithms mac hmac-sha2-512-etm@openssh.com hmac-sha2-256-etm@openssh.com umac-128-etm@openssh.com)"
  algo_note="filtered to what this OpenSSH supports"
  have ssh || algo_note="not filtered yet: OpenSSH is not installed (the apply run filters them)"
  ssh_version="$(ssh -V 2>&1 | sed -nE 's/^OpenSSH_([0-9]+)\.([0-9]+).*/\1 \2/p' || true)"
  rsa_line="# RequiredRSASize needs OpenSSH >= 9.1; the lockout guard already requires RSA keys >= 2048 bits"
  if [[ -n "$ssh_version" ]]; then
    read -r vmaj vmin <<<"$ssh_version"
    if (( vmaj > 9 || (vmaj == 9 && vmin >= 1) )); then rsa_line="RequiredRSASize 2048"; fi
  fi
  pw_value=no; auth_methods="AuthenticationMethods publickey"
  if [[ $ALLOW_PASSWORD -eq 1 ]]; then pw_value=yes; auth_methods="# AuthenticationMethods: any (--allow-password-auth)"; fi
  apply_file "$SSHD_DROPIN" 0644 root:root <<EOF
# Managed by scripts/hardening.sh (Synthetic Capital). Edit the script, not this file.
# sshd keeps the FIRST value it reads for each keyword, and this file sorts before the
# distribution's and cloud-init's drop-ins (e.g. 50-cloud-init.conf).
Port $SSH_PORT
PermitRootLogin no
AllowUsers ${ADMIN_USERS[*]}

# Keys only (password and keyboard-interactive logins only with --allow-password-auth).
PubkeyAuthentication yes
PasswordAuthentication $pw_value
KbdInteractiveAuthentication $pw_value
PermitEmptyPasswords no
HostbasedAuthentication no
IgnoreRhosts yes
$auth_methods
$rsa_line

# Few attempts, short grace time; idle sessions closed after 10 minutes.
MaxAuthTries 3
MaxSessions 4
LoginGraceTime 30
MaxStartups 10:30:60
ClientAliveInterval 300
ClientAliveCountMax 2

# No forwarding of any kind: this host is not a jump box.
DisableForwarding yes
AllowTcpForwarding no
AllowAgentForwarding no
AllowStreamLocalForwarding no
X11Forwarding no
PermitTunnel no
GatewayPorts no
PermitUserEnvironment no

StrictModes yes
UseDNS no
Compression no
# VERBOSE logs the fingerprint of the key used for each login (audit trail).
LogLevel VERBOSE

# Modern algorithms only ($algo_note).
KexAlgorithms $KEX
Ciphers $CIPHERS
MACs $MACS
EOF
  ssh_changed=$CHANGED
  if [[ $DRY_RUN -eq 1 ]]; then
    record sshd "$(outcome "$ssh_changed")" "$SSHD_DROPIN; port $SSH_PORT; validated with sshd -t before any reload"
  elif ! have sshd; then
    record sshd failed "sshd not found"
  else
    mkdir -p /run/sshd && chmod 0755 /run/sshd   # privilege separation dir (systemd makes it at runtime)
    [[ -n "$(find /etc/ssh -maxdepth 1 -name 'ssh_host_*_key' -print -quit)" ]] || ssh-keygen -A >/dev/null
    if ! out="$(sshd -t 2>&1)"; then
      # Never reload a configuration sshd rejects: restore the previous drop-in (or none).
      if [[ -e "$BACKUP_DIR$SSHD_DROPIN" ]]; then cp -a "$BACKUP_DIR$SSHD_DROPIN" "$SSHD_DROPIN"; else rm -f "$SSHD_DROPIN"; fi
      record sshd failed "sshd -t rejected the new configuration (rolled back): $out"
    else
      # Effective values: a Match block or an earlier drop-in could still override them.
      eff="$(sshd -T -C "user=${ADMIN_USERS[0]},host=localhost,addr=127.0.0.1" 2>/dev/null || true)"
      for kv in "permitrootlogin no" "passwordauthentication $pw_value" "port $SSH_PORT" "allowtcpforwarding no" "x11forwarding no" "maxauthtries 3"; do
        grep -qx -- "$kv" <<<"$eff" || warn "sshd: effective value differs from '$kv' (set elsewhere?): $(grep -m1 -- "^${kv%% *} " <<<"$eff" || true)"
      done
      if [[ $ssh_changed -eq 0 ]]; then
        record sshd unchanged "$SSHD_DROPIN (sshd -t ok)"
      elif [[ $HAVE_SYSTEMD -eq 0 ]]; then
        record sshd changed "$SSHD_DROPIN written, sshd -t ok; reload skipped (no systemd)"
      else
        if systemctl is-active --quiet ssh.socket; then
          # Socket activation (Ubuntu >= 22.10): the listening port comes from ssh.socket,
          # regenerated from sshd_config at daemon-reload. Open sessions are not affected.
          systemctl daemon-reload && systemctl restart ssh.socket
        elif systemctl is-active --quiet ssh; then systemctl reload ssh
        elif systemctl is-active --quiet sshd; then systemctl reload sshd
        else systemctl enable --now ssh; fi
        record sshd changed "$SSHD_DROPIN written, sshd -t ok, sshd reloaded"
        NEXT_STEPS+=("Open a NEW SSH session (port $SSH_PORT, key login) and check it before closing this one.")
      fi
    fi
  fi
fi

# ------------------------------------------------------------------------------------------
# 5. Deployment directory
# ------------------------------------------------------------------------------------------
# The compose CLI runs as the admin user (docker compose --env-file deploy/compose.env, a
# file without secrets); dockerd (root) bind-mounts secrets and certs. secrets/ is 0700 so
# no other host user can reach the files, whose owners and read bits stay as
# scripts/generate-secrets.sh made them (0400, owned by the container uid that reads each:
# 70 postgres, 65532 backend). nginx (uid 101) writes logs/nginx and reads certs/;
# certbot (root) writes acme/.
step "Deployment directory ($DEPLOY_DIR)"
NGINX_LOG_DIR="$DEPLOY_DIR/logs/nginx"
# Bind mounts keep numeric ids: whatever host account has the nginx uid can read the logs
# and the TLS key. Prefer a uid no host account uses (then set it in compose too).
if host_account="$(getent passwd "$NGINX_UID" | cut -d: -f1)" && [[ -n "$host_account" ]]; then
  warn "uid $NGINX_UID (nginx in the container) is the host account '$host_account', which can therefore read the nginx logs and TLS key; consider --nginx-uid with an unused uid and the same user: in docker-compose.yml"
fi
first_admin="${ADMIN_USERS[0]}"
admin_group="$(id -gn "$first_admin")"
deploy_changes=()
note_change() { if [[ $CHANGED -eq 1 ]]; then deploy_changes+=("$1"); fi; return 0; }
ensure_dir "$DEPLOY_DIR" 0750 "$first_admin:$admin_group"; note_change "$DEPLOY_DIR"
ensure_dir "$DEPLOY_DIR/secrets" 0700 "$first_admin:$admin_group"; note_change secrets/
if [[ -d "$DEPLOY_DIR/secrets" ]]; then
  while IFS= read -r -d '' f; do
    (( (8#$(stat -c '%a' "$f") & 8#022) != 0 )) || continue
    if [[ $DRY_RUN -eq 1 ]]; then info "would remove group/other write permission from $f"; else chmod go-w "$f"; info "removed group/other write permission from $f"; fi
    deploy_changes+=("${f#"$DEPLOY_DIR"/}")
  done < <(find "$DEPLOY_DIR/secrets" -type f -print0)
fi
ensure_mode "$DEPLOY_DIR/deploy/compose.env" 0640 "$first_admin:$admin_group"; note_change deploy/compose.env
ensure_dir "$DEPLOY_DIR/certs" 0755 root:root; note_change certs/
ensure_mode "$DEPLOY_DIR/certs/privkey.pem" 0400 "$NGINX_UID:$NGINX_GID"; note_change certs/privkey.pem
for d in acme acme/.well-known acme/.well-known/acme-challenge logs; do
  ensure_dir "$DEPLOY_DIR/$d" 0755 root:root; note_change "$d/"
done
ensure_dir "$NGINX_LOG_DIR" 0750 "$NGINX_UID:$NGINX_GID"; note_change logs/nginx/
for f in access.log error.log; do
  # Present from the start, so the fail2ban jails find their files before nginx ever ran.
  [[ -e "$NGINX_LOG_DIR/$f" ]] && continue
  if [[ $DRY_RUN -eq 1 ]]; then info "would create $NGINX_LOG_DIR/$f"; else install -m 0640 -o "$NGINX_UID" -g "$NGINX_GID" /dev/null "$NGINX_LOG_DIR/$f"; fi
  deploy_changes+=("logs/nginx/$f")
done
if [[ ${#deploy_changes[@]} -gt 0 ]]; then
  record deploy-dir "$(outcome 1)" "${deploy_changes[*]}"
else
  record deploy-dir unchanged "0750; secrets/ 0700; deploy/compose.env 0640; logs/nginx uid $NGINX_UID"
fi

# ------------------------------------------------------------------------------------------
# 6. fail2ban
# ------------------------------------------------------------------------------------------
# The nginx edge logs in UTC and its error log carries no zone: logtimezone = UTC. Web bans
# go into DOCKER-USER, because Docker DNATs published ports before INPUT; there the
# destination port is already the container's (8080/8443), so the nginx jails ban all ports.
step "fail2ban"
ssh_backend=auto
[[ -f /var/log/auth.log ]] || ssh_backend=systemd   # journald only (Debian 12 default)
f2b_changed=0
apply_file /etc/fail2ban/filter.d/sc-nginx-limit.conf 0644 root:root <<'EOF'
# Managed by scripts/hardening.sh (Synthetic Capital).
# limit_req / limit_conn rejections in the nginx edge's error.log, for example
#   2026/10/28 14:51:30 [error] 28#28: *67 limiting requests, excess: 11.000 by zone "auth", client: 203.0.113.7, server: ...
[Definition]
failregex = ^\s*\[(?:error|warn)\] \d+#\d+: \*\d+ limiting (?:requests, excess: [\d.]+|connections) by zone "[^"]+", client: <HOST>,
ignoreregex =
datepattern = {^LN-BEG}
EOF
f2b_changed=$(( f2b_changed | CHANGED ))
apply_file /etc/fail2ban/filter.d/sc-nginx-auth.conf 0644 root:root <<'EOF'
# Managed by scripts/hardening.sh (Synthetic Capital).
# Failed sign-ins (401 from the backend) in the nginx edge's access.log ("edge" format,
# which starts like "combined"), for example
#   203.0.113.7 - - [28/Oct/2026:14:51:30 +0000] "POST /api/v1/auth/login HTTP/2.0" 401 80 "-" "..." rt=...
[Definition]
failregex = ^<HOST> - \S+ \[\] "POST /api/v1/auth/login(?:\?[^ "]*)? HTTP/[\d.]+" 401
ignoreregex =
EOF
f2b_changed=$(( f2b_changed | CHANGED ))
apply_file /etc/fail2ban/jail.d/sc-hardening.local 0644 root:root <<EOF
# Managed by scripts/hardening.sh (Synthetic Capital).
[DEFAULT]
bantime = 1h
bantime.increment = true
bantime.maxtime = 1w
findtime = 10m
maxretry = 5
ignoreip = 127.0.0.1/8 ::1
banaction = iptables-multiport
banaction_allports = iptables-allports

[sshd]
enabled = true
port = $SSH_PORT
mode = aggressive
backend = $ssh_backend
maxretry = 4

# Requests the edge rejected with 429 (any rate-limit zone or connection limit).
[sc-nginx-limit]
enabled = true
filter = sc-nginx-limit
logpath = $NGINX_LOG_DIR/error.log
logtimezone = UTC
backend = auto
findtime = 2m
maxretry = 30
action = iptables-allports[name=sc-nginx-limit, chain="DOCKER-USER", protocol="all"]

# Repeated failed sign-ins from one address (the backend also locks the account).
[sc-nginx-auth]
enabled = true
filter = sc-nginx-auth
logpath = $NGINX_LOG_DIR/access.log
logtimezone = UTC
backend = auto
findtime = 10m
maxretry = 10
action = iptables-allports[name=sc-nginx-auth, chain="DOCKER-USER", protocol="all"]
EOF
f2b_changed=$(( f2b_changed | CHANGED ))
if [[ -f /lib/systemd/system/fail2ban.service || -f /usr/lib/systemd/system/fail2ban.service ]]; then
  apply_file /etc/systemd/system/fail2ban.service.d/sc-hardening.conf 0644 root:root <<'EOF'
# Managed by scripts/hardening.sh (Synthetic Capital).
# The nginx jails insert rules into Docker's DOCKER-USER chain: start after Docker.
[Unit]
After=docker.service
EOF
  f2b_changed=$(( f2b_changed | CHANGED ))
fi
if [[ $DRY_RUN -eq 1 ]]; then
  record fail2ban "$(outcome $f2b_changed)" "jails sshd (port $SSH_PORT), sc-nginx-limit, sc-nginx-auth (bans in DOCKER-USER)"
elif ! have fail2ban-client; then
  record fail2ban failed "fail2ban-client not found"
else
  # Filter self-test against known lines (catches regex drift between fail2ban versions),
  # then the full configuration test.
  t="$(mktemp -d)"
  printf '%s\n' '2026/10/28 14:51:30 [error] 28#28: *67 limiting requests, excess: 11.000 by zone "auth", client: 203.0.113.7, server: bank.example, request: "POST /api/v1/auth/login HTTP/2.0", host: "bank.example"' \
    '2026/10/28 14:51:44 [error] 28#28: *83 limiting connections by zone "conn_kyc", client: 203.0.113.8, server: bank.example, request: "POST /api/v1/kyc/verificar HTTP/2.0", host: "bank.example"' \
    '2026/10/28 14:51:45 [error] 28#28: *84 client intended to send too large body: 70000 bytes, client: 203.0.113.8, server: bank.example' > "$t/error.log"
  printf '%s\n' '203.0.113.9 - - [28/Oct/2026:14:51:30 +0000] "POST /api/v1/auth/login HTTP/2.0" 401 80 "-" "curl/8" rt=0.001 uct="0.001" urt="0.001" us="401" rid=0123456789abcdef0123456789abcdef host="bank.example" tls=TLSv1.3' \
    '203.0.113.9 - - [28/Oct/2026:14:51:31 +0000] "POST /api/v1/auth/login HTTP/2.0" 200 80 "-" "curl/8" rt=0.001 uct="0.001" urt="0.001" us="200" rid=0123456789abcdef0123456789abcdef host="bank.example" tls=TLSv1.3' > "$t/access.log"
  m1="$(fail2ban-regex "$t/error.log" /etc/fail2ban/filter.d/sc-nginx-limit.conf 2>/dev/null | sed -nE 's/^Lines: .* ([0-9]+) matched.*/\1/p' || true)"
  m2="$(fail2ban-regex "$t/access.log" /etc/fail2ban/filter.d/sc-nginx-auth.conf 2>/dev/null | sed -nE 's/^Lines: .* ([0-9]+) matched.*/\1/p' || true)"
  rm -rf "$t"
  if [[ "$m1" != 2 || "$m2" != 1 ]]; then
    record fail2ban failed "filter self-test failed (limit matched ${m1:-?} of 2 lines, auth ${m2:-?} of 1)"
  elif ! out="$(fail2ban-client -t 2>&1)"; then
    record fail2ban failed "fail2ban-client -t: $(tail -n 2 <<<"$out" | tr '\n' ' ')"
  elif [[ $HAVE_SYSTEMD -eq 0 ]]; then
    record fail2ban "$(outcome $f2b_changed)" "config test ok, filter self-test ok; service start skipped (no systemd)"
  else
    systemctl daemon-reload
    systemctl enable fail2ban >/dev/null 2>&1 || true
    if [[ $f2b_changed -eq 1 ]] || ! systemctl is-active --quiet fail2ban; then systemctl restart fail2ban; fi
    sleep 2
    if systemctl is-active --quiet fail2ban; then
      record fail2ban "$(outcome $f2b_changed)" "running; jails: $(fail2ban-client status 2>/dev/null | sed -n 's/.*Jail list:[[:space:]]*//p')"
    else
      record fail2ban failed "fail2ban did not start (journalctl -u fail2ban)"
    fi
    if have iptables && ! iptables -n -L DOCKER-USER >/dev/null 2>&1; then
      warn "no DOCKER-USER chain yet (Docker not running?): the nginx jails ban once Docker is up and fail2ban has restarted"
    fi
  fi
fi

# ------------------------------------------------------------------------------------------
# 7. Kernel parameters
# ------------------------------------------------------------------------------------------
# net.ipv4.ip_forward stays as it is: Docker needs it. rp_filter=1 (strict) suits a
# single-homed host; use 2 (loose) where routing is asymmetric (several NICs).
step "sysctl"
SYSCTL_FILE=/etc/sysctl.d/60-sc-hardening.conf
apply_file "$SYSCTL_FILE" 0644 root:root <<'EOF'
# Managed by scripts/hardening.sh (Synthetic Capital).
# Network: no ICMP redirects or source routing, reverse-path filtering, SYN cookies,
# martians logged. (net.ipv4.ip_forward is left to Docker.)
net.ipv4.tcp_syncookies = 1
net.ipv4.tcp_rfc1337 = 1
net.ipv4.conf.all.accept_redirects = 0
net.ipv4.conf.default.accept_redirects = 0
net.ipv4.conf.all.secure_redirects = 0
net.ipv4.conf.default.secure_redirects = 0
net.ipv4.conf.all.send_redirects = 0
net.ipv4.conf.default.send_redirects = 0
net.ipv6.conf.all.accept_redirects = 0
net.ipv6.conf.default.accept_redirects = 0
net.ipv4.conf.all.accept_source_route = 0
net.ipv4.conf.default.accept_source_route = 0
net.ipv6.conf.all.accept_source_route = 0
net.ipv6.conf.default.accept_source_route = 0
net.ipv4.conf.all.rp_filter = 1
net.ipv4.conf.default.rp_filter = 1
net.ipv4.conf.all.log_martians = 1
net.ipv4.conf.default.log_martians = 1
net.ipv4.icmp_echo_ignore_broadcasts = 1
net.ipv4.icmp_ignore_bogus_error_responses = 1
# Kernel pointers and the kernel log hidden from unprivileged users; no setuid core dumps.
kernel.kptr_restrict = 2
kernel.dmesg_restrict = 1
fs.suid_dumpable = 0
EOF
sysctl_changed=$CHANGED
if [[ $DRY_RUN -eq 1 ]]; then
  record sysctl "$(outcome "$sysctl_changed")" "$SYSCTL_FILE"
else
  errs="$(sysctl -p "$SYSCTL_FILE" 2>&1 >/dev/null || true)"
  n_err="$(grep -c '^sysctl:' <<<"$errs" || true)"
  n_keys="$(grep -c '^[a-z]' "$SYSCTL_FILE")"
  if [[ "$n_err" -eq 0 ]]; then
    record sysctl "$(outcome "$sysctl_changed")" "$SYSCTL_FILE applied ($n_keys keys)"
  elif [[ "$n_err" -ge "$n_keys" ]] && grep -q 'Read-only file system' <<<"$errs"; then
    record sysctl skipped "$SYSCTL_FILE written ($n_keys keys); not applied here: /proc/sys is read-only (container?); applied at boot"
  else
    record sysctl partial "$SYSCTL_FILE applied except $n_err keys: $(head -n 1 <<<"$errs")"
  fi
fi

# ------------------------------------------------------------------------------------------
# 8. Docker daemon defaults
# ------------------------------------------------------------------------------------------
# no-new-privileges: the default for every container. live-restore: containers keep running
# while dockerd restarts (upgrades). userland-proxy=false: published ports go through
# iptables DNAT only, which keeps the real client address (nginx logs, rate limits,
# fail2ban); IPv6 clients then reach published ports only through an IPv6-enabled network,
# so publish an AAAA record only together with IPv6 on the edge network. icc=false blocks
# container-to-container traffic on the DEFAULT bridge (docker0) only: compose's user-defined
# networks are unaffected, so it is compatible with them and only hardens containers started
# without a network. json-file logs rotate at 10 MB x 5.
step "Docker daemon"
DAEMON_JSON=/etc/docker/daemon.json
if [[ $SKIP_DOCKER -eq 1 ]]; then
  record docker skipped "--skip-docker"
elif ! have python3; then
  record docker "$([[ $DRY_RUN -eq 1 ]] && echo would-change || echo failed)" "python3 is needed to merge $DAEMON_JSON (the packages step installs it)"
else
  current="{}"
  [[ -f "$DAEMON_JSON" ]] && current="$(cat "$DAEMON_JSON")"
  # Keys already there (registry mirrors, data-root, ...) are kept; ours are set.
  if ! merged="$(SC_CURRENT="$current" python3 -c '
import json, os, sys
try:
    cfg = json.loads(os.environ["SC_CURRENT"].strip() or "{}")
except ValueError as e:
    sys.exit("invalid JSON: %s" % e)
cfg.update({"no-new-privileges": True, "live-restore": True, "userland-proxy": False, "icc": False})
if cfg.get("log-driver", "json-file") == "json-file":
    cfg["log-driver"] = "json-file"
    opts = dict(cfg.get("log-opts", {}))
    opts.update({"max-size": "10m", "max-file": "5"})
    cfg["log-opts"] = opts
print(json.dumps(cfg, indent=2, sort_keys=True))
' 2>&1)"; then
    record docker failed "$DAEMON_JSON: $merged; fix it first"
  else
    apply_file "$DAEMON_JSON" 0644 root:root <<<"$merged"
    docker_changed=$CHANGED
    if [[ $DRY_RUN -eq 1 ]]; then
      record docker "$(outcome "$docker_changed")" "$DAEMON_JSON"
    elif have dockerd && ! out="$(dockerd --validate --config-file "$DAEMON_JSON" 2>&1)"; then
      if [[ -e "$BACKUP_DIR$DAEMON_JSON" ]]; then cp -a "$BACKUP_DIR$DAEMON_JSON" "$DAEMON_JSON"; else rm -f "$DAEMON_JSON"; fi
      record docker failed "dockerd --validate rejected it (rolled back): $out"
    elif [[ $docker_changed -eq 0 ]]; then
      record docker unchanged "$DAEMON_JSON"
    elif ! have dockerd || ! svc_active docker; then
      record docker changed "$DAEMON_JSON written; applies when dockerd starts$(have dockerd || echo ' (Docker not installed yet)')"
    elif [[ -z "$(docker ps -q 2>/dev/null || true)" ]]; then
      systemctl restart docker
      record docker changed "$DAEMON_JSON written; dockerd restarted (no containers were running)"
    else
      # live-restore can be reloaded: activate it now so the later restart keeps containers up.
      systemctl reload docker || true
      record docker changed "$DAEMON_JSON written; dockerd reloaded; restart pending"
      NEXT_STEPS+=("Restart Docker in a maintenance window for userland-proxy/icc/log defaults: docker compose down && systemctl restart docker && docker compose up -d")
    fi
  fi
fi

# ------------------------------------------------------------------------------------------
# 9. Time synchronisation (chrony)
# ------------------------------------------------------------------------------------------
step "Time (chrony)"
if ! have chronyd; then
  record chrony "$([[ $DRY_RUN -eq 1 ]] && echo would-change || echo failed)" "chrony not installed$([[ $DRY_RUN -eq 1 ]] && echo ' yet (packages step)')"
elif [[ $HAVE_SYSTEMD -eq 0 ]]; then
  record chrony skipped "installed; service not started (no systemd)"
elif svc_active chrony && systemctl is-enabled --quiet chrony; then
  record chrony unchanged "running; leap status $(chronyc -n tracking 2>/dev/null | sed -n 's/^Leap status *: //p')"
elif [[ $DRY_RUN -eq 1 ]]; then
  info "would run: systemctl enable --now chrony"
  record chrony would-change "would enable and start chrony"
else
  systemctl enable --now chrony
  record chrony changed "enabled and started"
fi

# ------------------------------------------------------------------------------------------
# 10. Audit rules (auditd)
# ------------------------------------------------------------------------------------------
step "Audit (auditd)"
AUDIT_RULES=/etc/audit/rules.d/60-sc-hardening.rules
audit_rules() {
  echo "## Managed by scripts/hardening.sh (Synthetic Capital): baseline audit rules."
  local w path perm key
  # path, permissions, key. Watches only for paths that exist: auditctl rejects the others.
  while read -r path perm key; do
    [[ -e "$path" ]] && echo "-w $path -p $perm -k $key"
  done <<EOF
/etc/passwd wa identity
/etc/group wa identity
/etc/shadow wa identity
/etc/gshadow wa identity
/etc/sudoers wa privilege
/etc/sudoers.d wa privilege
/etc/ssh/sshd_config wa sshd
/etc/ssh/sshd_config.d wa sshd
/etc/docker wa docker
/etc/ufw wa firewall
/etc/fail2ban wa firewall
/etc/audit wa audit-config
/etc/localtime wa time-change
$DEPLOY_DIR/secrets rwa sc-secrets
$DEPLOY_DIR/deploy/compose.env wa sc-deploy
$DEPLOY_DIR/docker-compose.yml wa sc-deploy
$DEPLOY_DIR/certs wa sc-certs
EOF
  for w in docker dockerd containerd sudo su insmod rmmod modprobe; do
    path="$(command -v "$w" 2>/dev/null || true)"
    if [[ -n "$path" && -e "$path" ]]; then echo "-w $(readlink -f "$path") -p x -k exec-$w"; fi
  done
  echo "-a always,exit -F arch=b64 -S adjtimex,settimeofday,clock_settime -k time-change"
  echo "-a always,exit -F arch=b64 -S init_module,finit_module,delete_module -k modules"
  if [[ "$(uname -m)" == x86_64 ]]; then
    echo "-a always,exit -F arch=b32 -S adjtimex,settimeofday,clock_settime,stime -k time-change"
  fi
  return 0
}
apply_file "$AUDIT_RULES" 0640 root:root < <(audit_rules)
audit_changed=$CHANGED
if [[ $DRY_RUN -eq 1 ]]; then
  record auditd "$(outcome "$audit_changed")" "$AUDIT_RULES"
elif ! have augenrules; then
  record auditd failed "augenrules not found (auditd not installed?)"
elif auditctl -s >/dev/null 2>&1; then
  out="$(augenrules --load 2>&1 || true)"
  if grep -qi 'error' <<<"$out"; then
    record auditd failed "augenrules --load: $(tail -n 1 <<<"$out")"
  else
    if [[ $HAVE_SYSTEMD -eq 1 ]]; then systemctl enable --now auditd >/dev/null 2>&1 || true; fi
    record auditd "$(outcome "$audit_changed")" "$AUDIT_RULES loaded ($(auditctl -l 2>/dev/null | wc -l) rules active)"
  fi
else
  augenrules >/dev/null 2>&1 || true   # compiles /etc/audit/audit.rules without loading it
  record auditd skipped "$AUDIT_RULES installed ($(grep -c '^-' "$AUDIT_RULES") rules); no kernel audit access here (container?): loaded at boot"
fi

# ------------------------------------------------------------------------------------------
# 11. Log rotation for the nginx edge logs
# ------------------------------------------------------------------------------------------
# copytruncate: nginx keeps its file open (O_APPEND), so the container needs no signal.
step "Log rotation"
apply_file /etc/logrotate.d/sc-nginx 0644 root:root <<EOF
# Managed by scripts/hardening.sh (Synthetic Capital): nginx edge logs on the host volume.
$NGINX_LOG_DIR/*.log {
    daily
    rotate 30
    compress
    delaycompress
    missingok
    notifempty
    copytruncate
}
EOF
lr_changed=$CHANGED
lr_out=""
if [[ $DRY_RUN -eq 0 ]] && have logrotate && [[ -d "$NGINX_LOG_DIR" ]]; then
  lr_out="$(logrotate -d /etc/logrotate.d/sc-nginx 2>&1 || echo 'error: logrotate -d failed')"
fi
if grep -qi '^error' <<<"$lr_out"; then
  record logrotate failed "logrotate -d: $(grep -m1 -i '^error' <<<"$lr_out")"
else
  record logrotate "$(outcome "$lr_changed")" "/etc/logrotate.d/sc-nginx (daily, 30 days)"
fi

# ------------------------------------------------------------------------------------------
# Summary
# ------------------------------------------------------------------------------------------
say ""
say "== Summary: $OS_NAME, $([[ $DRY_RUN -eq 1 ]] && echo 'DRY RUN (nothing was changed)' || echo applied), systemd: $([[ $HAVE_SYSTEMD -eq 1 ]] && echo yes || echo no)"
printf '  %-20s %-13s %s\n' STEP RESULT DETAIL
for i in "${!RESULT_STEP[@]}"; do
  printf '  %-20s %-13s %s\n' "${RESULT_STEP[$i]}" "${RESULT_STATUS[$i]}" "${RESULT_DETAIL[$i]}"
done
if [[ $DRY_RUN -eq 0 && -d "$BACKUP_DIR" ]]; then say "  Previous versions of the files changed: $BACKUP_DIR"; fi
NEXT_STEPS+=("Docker-published ports bypass UFW: the compose file must publish only nginx (80, 443).")
if [[ ${#WARNINGS[@]} -gt 0 ]]; then
  say "Warnings:"; for w in "${WARNINGS[@]}"; do say "  ! $w"; done
fi
say "Next steps:"; for n in "${NEXT_STEPS[@]}"; do say "  - $n"; done
exit "$FAILED"
