# Synthetic Capital

A 100%-reserve commercial bank (Rothbardian model) for Mexico. Fractional
reserve is impossible by construction: the rules are enforced by PostgreSQL
(triggers, CHECK constraints and per-role privileges) and checked again by
the Rust backend.

| Entity | What it does | Rule enforced in the database |
|---|---|---|
| **IFPE** (sight vault) | Deposits, withdrawals, transfers | Client liabilities == liquid reserve in custody, checked at every COMMIT (`SC_RESERVA_100`). This money is never lent. |
| **SOFOM** (credit) | Fixed-term mutuum contracts and loans | A loan is disbursed only if 100% funded by mutuums maturing **on or after** the loan (`SC_CALCE_PLAZOS`, `SC_FONDEO_INCOMPLETO`). No early withdrawal. |
| **Isolation** | | Separate `ifpe` / `sofom` schemas and roles; every transaction runs under `SET LOCAL ROLE`. The bridge role cannot touch loans. |

Further rules enforced in the database (migration `0003`):

- **SOFOM liquidity:** cash always covers the unlent principal of every active
  mutuum. Interest and loan losses are paid from SOFOM capital, never from
  another investor's money.
- **Write-offs:** a defaulted loan can be written off only when capital
  absorbs the loss. Its funding mutuums are then released and repaid in full.
- **Value dates:** mutuums start and loans are disbursed *today*. No
  back-dating is allowed, so no-early-withdrawal cannot be bypassed.
- **Sessions:** every request rechecks the JWT's role and revocation state
  against the database.

Integrity checks (migration `0004` and the backend):

- **Balances are bound to the ledger.** Shadow balances are written only by
  the ledger's insert triggers. At every COMMIT a balance that the ledger does
  not justify is rejected (`SC_SALDO_SIN_LEDGER`), even if the reserve totals
  still match.
- **Reserve cuts cannot be forged.** The database rejects a cut whose leaves
  differ from the live balances, whose leaf hashes are wrong, or whose ledger
  anchor is not the current head or moves backwards. The backend also replays
  the whole ledger before publishing a cut.
- **Truncation is detectable.** Every cut publishes `(secuencia_ledger,
  hash_ledger)`, and the full anchor history is public. The chain audit checks
  every anchor, replays every balance and checks the IFPE<->SOFOM mirror
  entries and each mutuum's terms hash.
- **Proofs are checked in the browser.** The dashboard verdict requires that
  the leaf is the customer's own account and that the cut matches the public,
  unauthenticated copy (`GET /reserva/cortes/{id}`).

## SPEI (interbank transfers, migration `0005`)

Real transfers through a settlement partner such as STP. SPEI moves only the
IFPE vault and its custody reserve; the SOFOM never sees it.

- **Receiving:** `POST /api/v1/spei/webhook/inbound`, signed with HMAC-SHA256
  (`X-Spei-Firma: t=<unix>,v1=<hex>` over `<t>.<raw body>`, 5-minute window,
  constant-time check, two secrets during rotation). In one transaction:
  the client's balance and the custody reserve go up by the same amount, the
  DEPOSITO enters the hash chain, and the payment is logged in
  `ifpe.spei_inbound`. The reply follows STP: `confirmar`, or `devolver`
  with the Banxico cause when the CLABE is unknown (1), the account is blocked
  or has no KYC (2), or it is closed (3). Authenticated notifications that
  cannot be used are quarantined and returned (14), never answered with a 4xx.
- **No double credit:** a payment's identity is issuing participant + tracking
  key + operation date (Banxico makes the tracking key unique per participant
  per day). A unique index plus a per-identity advisory lock mean concurrent
  or repeated webhooks credit once. A second check blocks the same payment
  under another identity (an operator typing the calendar date or a wrong
  issuer code, or credits from before 0005).
- **Sending:** `POST /api/v1/spei/transfer`. The balance and the reserve go
  down at once (the money is committed to the order and cannot be spent
  twice), and the order is dispatched by a background worker. The worker
  never refunds on a timeout or an ambiguous answer. A rejection counts as
  final only on the first attempt; afterwards (for example "duplicate
  tracking key" on a resend) the order goes to manual review. Refunds happen
  only on a first-attempt rejection, a cancellation or a return, exactly once
  (`spei-reverso:<order>`), even when the status webhook, the poller and an
  incoming return all report it. Settled orders are polled for returns during
  `SPEI_VENTANA_DEVOLUCION_DIAS`.
- **Database guarantees:** every new DEPOSITO is a logged SPEI or an order
  reversal, and every new RETIRO is an order or an operator correction, each
  created in the same transaction as its record. A separate role (`sc_spei`)
  is the only one that can log incoming payments, change order states or
  refund. Client-facing code (`sc_ifpe`) can only create orders. Account
  owners and CLABEs never change.
- **CLABE:** official mod-10 check digit (weights 3-7-1) in Rust and in
  PostgreSQL. CLABEs live in `ifpe.clabes_asignadas` (STP virtual CLABEs
  `646...` included) and are never reassigned.
- **Reconciliation:** `GET /api/v1/spei/conciliacion` reports, per custody
  account, book reserve plus orders not yet confirmed as settled.
- **Corrections:** `POST /api/v1/operaciones/spei/recibidos/{id}/correccion`
  (operator) reverses a wrong credit, up to the credited amount.

`backend/src/spei/pasarela.rs` defines the gateway interface. The simulated
gateway reproduces STP's behaviour, including duplicate rejections on resend
and delayed status queries. A production STP client (RSA-signed orders,
participant catalog) plugs in there.

## Layout

```
migrations/   SQL schema (sqlx-cli); 0001 approved and immutable
backend/      Rust: axum + sqlx + tokio
  src/db.rs              role-scoped transactions (the only place that calls begin())
  src/ledger/            IFPE engine + SHA-256 hash-chain verifier
  src/crypto/merkle.rs   Merkle sum tree (Proof of Reserve)
  src/reserva.rs         reserve cuts and per-client inclusion proofs
  src/sofom/             IFPE -> mutuum bridge, maturity liquidation, loans
  src/routes/            HTTP API (/api/v1)
frontend/     Next.js dashboard (Task 3)
tests/db/     SQL invariant tests for the schema
```

## Running locally

```bash
docker compose up -d postgres                 # or a local PostgreSQL 16
cp .env.example .env                          # set JWT_SECRET; MODO_DEMO=true for the demo
cd backend && cargo run                       # applies migrations and starts on :8080
```

Tests (need a user with `CREATEDB` and `CREATEROLE`):

```bash
cd backend && DATABASE_URL=postgres://synthetic:synthetic@localhost:5432/synthetic_capital cargo test
# SQL invariants: run on an empty, freshly migrated database (everything is rolled back).
psql "$EMPTY_DATABASE_URL" -f tests/db/0001_invariantes.sql
cd frontend && npm test && npm run build
FRONTEND_URL=http://localhost:3000 node e2e/smoke.mjs   # needs backend (MODO_DEMO=true) + next start
```

### Production database users

By default the backend connects as the schema owner, which can disable
triggers. It logs a warning at startup when it does. In production, migrate as
the owner and serve as a login that can act only through the `sc_*` roles:

```sql
CREATE ROLE sc_app LOGIN PASSWORD '...' NOINHERIT;
GRANT sc_ifpe, sc_sofom, sc_puente, sc_auditor, sc_kyc, sc_spei TO sc_app;
```

```bash
DATABASE_URL_PROPIETARIO=postgres://owner:...@db/synthetic_capital   # migrations + bootstrap only
DATABASE_URL=postgres://sc_app:...@db/synthetic_capital              # the running service
```

### HTTP limits

The backend serves HTTP/1.1 itself with these limits; put a TLS reverse proxy
in front for production.

| Variable | Default | Effect |
|---|---|---|
| `HTTP_CABECERAS_SEG` | 10 | Close connections that do not send a full request head in time, including idle keep-alive and silent sockets |
| `HTTP_CUERPO_SEG` | 15 | Request body deadline (408 `TIEMPO_AGOTADO`) |
| `HTTP_PETICION_SEG` | 30 | Whole-request deadline (503 `TIEMPO_AGOTADO`; the transaction rolls back) |
| `HTTP_MAX_CONEXIONES` | 512 | Concurrent connections; beyond this the server stops accepting instead of running out of file descriptors |
| `HTTP_APAGADO_SEG` | 20 | Graceful shutdown deadline |

Request bodies are limited to 64 KB.

## API (summary)

Amounts travel as decimal **strings** (`"1500.00"`), never floats; a JSON
number is rejected with 400. Every operation that moves money requires a
`clave_idempotencia` (8–64 characters, one UUID per operation). A new
operation returns 201; a repeat with the same key returns 200 with the
original movement. Reusing a key with different data returns 409.

Every error, including unknown routes (404), wrong methods (405), non-JSON
bodies (415), oversized bodies (413) and slow bodies (408), uses one
envelope: `{"error": {"codigo": "...", "mensaje": "..."}}`. Business rules
return 422. A database invariant violation (for example `SC_RESERVA_100`)
returns 500 `INTEGRIDAD` and is logged at ERROR: it means a bug or tampering,
never a normal rejection.

Login allows 5 attempts per 15-minute window. Each attempt is counted before
the password is hashed, so a parallel burst cannot test more than 5 passwords.

| Method | Route | Access |
|---|---|---|
| POST | `/api/v1/auth/registro`, `/api/v1/auth/login` | public |
| GET | `/api/v1/tablero` | client: vault + investment + live reserve |
| GET | `/api/v1/ifpe/cuenta`, `/api/v1/ifpe/movimientos` | client |
| POST | `/api/v1/ifpe/transferencias` | client: internal transfer |
| POST | `/api/v1/ifpe/retiros` | client: withdrawal to own external CLABE (an outgoing SPEI) |
| POST | `/api/v1/ifpe/depositos/simulados` | client, `MODO_DEMO` only |
| GET | `/api/v1/sofom/tasas` | public: terms and rates |
| GET | `/api/v1/sofom/posicion` | client |
| POST | `/api/v1/sofom/mutuos` | client: **IFPE -> SOFOM bridge** |
| POST | `/api/v1/sofom/mutuos/{id}/liquidar` | owning client or operator (on maturity) |
| GET | `/api/v1/reserva/estado`, `/api/v1/reserva/cortes/ultimo` | public |
| GET | `/api/v1/reserva/cortes?antes=&limite=`, `/api/v1/reserva/cortes/{id}` | public: history of published anchors (root, totals, ledger sequence and hash) |
| GET | `/api/v1/reserva/prueba` | client: Merkle proof for their account |
| POST | `/api/v1/reserva/verificar` | public: verify a proof against the published root |
| GET | `/api/v1/auditoria/ledger/{ifpe,sofom}` | operator / auditor |
| POST | `/api/v1/operaciones/depositos` | operator: manual entry of an incoming SPEI (5-digit `banco_emisor`, `fecha_operacion`, `clave_rastreo`); same path and same identity as the webhook, so a payment entered by hand and notified by the provider is credited once |
| POST | `/api/v1/operaciones/creditos`, `.../{id}/pagos`, `.../{id}/castigo` | operator: loans, collections, write-offs (losses absorbed by SOFOM capital) |
| POST | `/api/v1/operaciones/usuarios/{id}/revocar-sesiones` | operator: invalidate a user's JWTs immediately |
| POST | `/api/v1/spei/webhook/inbound`, `/api/v1/spei/webhook/estado` | SPEI provider (HMAC signature) |
| POST | `/api/v1/spei/transfer` | client: send an SPEI |
| GET | `/api/v1/spei/transferencias`, `/api/v1/spei/recibidos` | client |
| GET | `/api/v1/spei/conciliacion` | operator / auditor |
| POST | `/api/v1/operaciones/spei/recibidos/{id}/correccion` | operator: reverse a wrong SPEI credit |
| POST | `/api/v1/operaciones/...` | operator: holds, KYC, AML alerts, capital |

Full table in `backend/src/routes/mod.rs`.
