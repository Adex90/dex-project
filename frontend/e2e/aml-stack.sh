#!/usr/bin/env bash
# Stack for e2e/aml.mjs: a fresh database, the backend on :8080 (MODO_DEMO, two
# CUMPLIMIENTO officers, an operator and an auditor) and `next start` on :3000.
# Staff enroll two-step verification on their first sign-in (aml.mjs does it).
#
#   e2e/aml-stack.sh start   # SKIP_FRONTEND_BUILD=1 reuses frontend/.next from the same commit
#   e2e/aml-stack.sh stop    # kills both servers by PID and drops the database
#
# Needs PostgreSQL with a user that can create databases (PG* variables below)
# and ports 8080 and 3000 free. PIDs and logs go to $AML_E2E_DIR.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
DIR="${AML_E2E_DIR:-${TMPDIR:-/tmp}/sc-aml-e2e}"
DB="${DB:-sc_e2e_aml}"
export PGHOST="${PGHOST:-localhost}" PGUSER="${PGUSER:-synthetic}" PGPASSWORD="${PGPASSWORD:-synthetic}"
mkdir -p "$DIR"

stop() {
  for f in frontend backend; do
    if [ -f "$DIR/$f.pid" ]; then kill "$(cat "$DIR/$f.pid")" 2>/dev/null || true; rm -f "$DIR/$f.pid"; fi
  done
  sleep 2
  psql -d postgres -q -c "DROP DATABASE IF EXISTS $DB WITH (FORCE)"
}

start() {
  for p in 8080 3000; do
    if (exec 3<>"/dev/tcp/127.0.0.1/$p") 2>/dev/null; then echo "port $p is busy" >&2; exit 1; fi
  done
  psql -d postgres -v ON_ERROR_STOP=1 -q -c "DROP DATABASE IF EXISTS $DB" -c "CREATE DATABASE $DB ENCODING 'UTF8' TEMPLATE template0"

  cat > "$DIR/stack.env" <<ENV
DATABASE_URL=postgres://$PGUSER:$PGPASSWORD@$PGHOST:5432/$DB
MIGRAR_AL_INICIO=true
MODO_DEMO=true
BIND_ADDR=127.0.0.1:8080
FRONTEND_ORIGIN=http://localhost:3000
CORTE_INTERVALO_SEG=0
LIQUIDACION_INTERVALO_SEG=0
CORTE_MIN_INTERVALO_SEG=0
SPEI_DESPACHO_INTERVALO_MS=0
RUST_LOG=info
JWT_SECRET=$(openssl rand -hex 32)
MFA_CLAVE_CIFRADO=$(openssl rand -hex 32)
SPEI_WEBHOOK_SECRETO=$(openssl rand -hex 32)
BOOTSTRAP_CUMPLIMIENTO_EMAILS=oficial1@cumplimiento.mx,oficial2@cumplimiento.mx
BOOTSTRAP_CUMPLIMIENTO_PASSWORD=cumplimiento-e2e-2026
BOOTSTRAP_OPERADOR_EMAIL=operador@synthetic.mx
BOOTSTRAP_OPERADOR_PASSWORD=operador-e2e-2026
ENV

  ( cd "$REPO/backend" && cargo build -q )
  ( set -a; . "$DIR/stack.env"; set +a; cd "$REPO/backend"; exec ./target/debug/synthetic-capital ) > "$DIR/backend.log" 2>&1 &
  echo $! > "$DIR/backend.pid"
  for _ in $(seq 1 120); do curl -sf http://127.0.0.1:8080/salud >/dev/null && break; sleep 1; done
  curl -sf http://127.0.0.1:8080/salud >/dev/null || { echo "backend did not start; see $DIR/backend.log" >&2; exit 1; }

  # Auditor: the bootstrap only creates officers and the operator, so register a
  # client and promote it as the schema owner.
  curl -sf -X POST http://127.0.0.1:8080/api/v1/auth/registro -H 'content-type: application/json' \
    -d '{"tipo_persona":"FISICA","nombre":"Auditoria","apellido_paterno":"Interna","email":"auditor@synthetic.mx","password":"auditor-e2e-2026"}' >/dev/null
  psql -d "$DB" -v ON_ERROR_STOP=1 -q -c \
    "UPDATE kyc.credenciales SET rol = 'AUDITOR' WHERE cliente_id = (SELECT id FROM kyc.clientes WHERE email = 'auditor@synthetic.mx')"

  if [ "${SKIP_FRONTEND_BUILD:-0}" != 1 ]; then
    ( cd "$REPO/frontend" && NEXT_PUBLIC_API_URL=http://localhost:8080 npm run build > "$DIR/frontend-build.log" 2>&1 )
  fi
  ( cd "$REPO/frontend"; exec ./node_modules/.bin/next start --port 3000 ) > "$DIR/frontend.log" 2>&1 &
  echo $! > "$DIR/frontend.pid"
  for _ in $(seq 1 60); do curl -sf -o /dev/null http://localhost:3000/ && break; sleep 1; done
  echo "backend pid $(cat "$DIR/backend.pid"), frontend pid $(cat "$DIR/frontend.pid"); logs in $DIR"
}

case "${1:-}" in
  start) start ;;
  stop) stop ;;
  *) echo "usage: $0 start|stop" >&2; exit 2 ;;
esac
