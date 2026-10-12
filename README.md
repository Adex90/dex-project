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
psql "$DATABASE_URL" -f tests/db/0001_invariantes.sql
```

## API (summary)

Amounts travel as decimal **strings** (`"1500.00"`), never floats. Every
operation that moves money requires a `clave_idempotencia` (8–64 characters,
one UUID per operation). A new operation returns 201; a repeat with the same
key returns 200 with the original movement.

| Method | Route | Access |
|---|---|---|
| POST | `/api/v1/auth/registro`, `/api/v1/auth/login` | public |
| GET | `/api/v1/tablero` | client: vault + investment + live reserve |
| GET | `/api/v1/ifpe/cuenta`, `/api/v1/ifpe/movimientos` | client |
| POST | `/api/v1/ifpe/transferencias`, `/api/v1/ifpe/retiros` | client |
| POST | `/api/v1/ifpe/depositos/simulados` | client, `MODO_DEMO` only |
| GET | `/api/v1/sofom/tasas` | public: terms and rates |
| GET | `/api/v1/sofom/posicion` | client |
| POST | `/api/v1/sofom/mutuos` | client: **IFPE -> SOFOM bridge** |
| POST | `/api/v1/sofom/mutuos/{id}/liquidar` | owning client or operator (on maturity) |
| GET | `/api/v1/reserva/estado`, `/api/v1/reserva/cortes/ultimo` | public |
| GET | `/api/v1/reserva/prueba` | client: Merkle proof for their account |
| POST | `/api/v1/reserva/verificar` | public: verify a proof against the published root |
| GET | `/api/v1/auditoria/ledger/{ifpe,sofom}` | operator / auditor |
| POST | `/api/v1/operaciones/...` | operator: SPEI, holds, KYC, AML alerts, loans, capital |

Full table in `backend/src/routes/mod.rs`.
