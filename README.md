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

## SPEI (interbank transfers, migrations `0005` and `0008`)

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
  A return (`tipoPago` 0, 16, 17, 18, 23, 24, or any notification carrying
  `claveRastreoDevolucion`) is applied to its order, which is refunded once;
  the database refuses to book a return as an ordinary credit, and requires
  it to come back to the order's originating CLABE for the amount sent. A
  return that matches no order, or whose reversal is impossible (closed
  account: the order is flagged for manual review), is quarantined and
  answered `confirmar` (`CUARENTENA`): SPEI does not allow returning a return.
  An operator can enter a return by hand (`clave_rastreo_devolucion` on
  `POST /api/v1/operaciones/depositos`) only for a settled order that the
  provider, queried at that moment, reports as returned, and never for their
  own order. A later notification with the same identity that names another
  order is held for review, not confirmed as a repeat.
- **No double credit:** a payment's identity is issuing participant + tracking
  key + operation date (Banxico makes the tracking key unique per participant
  per day). A unique index plus a per-identity advisory lock mean concurrent
  or repeated webhooks credit once. A second check blocks the same payment
  under another identity (an operator typing the calendar date or a wrong
  issuer code, or credits from before 0005); two identities reported by the
  provider are never duplicates of each other. Migration `0008` logs pre-0005
  credits with a canonical identity (origin `LEGADO`), so a new notification of
  the same payment is an idempotent repeat. A webhook that hits the
  possible-duplicate check, or an identity already logged with another amount
  or CLABE, is held without crediting and answered `confirmar`
  (`EN_REVISION`); `GET /api/v1/spei/en-revision` (operator / auditor) lists
  what is still open. The operator resolves it by entering the payment with
  `distinto_de` (the earlier transaction named in the 409) and a `motivo`, by
  voiding a wrong manual entry (below), or by discarding a held notification
  that really is a duplicate:
  `POST /api/v1/operaciones/spei/en-revision/{id}/descarte` (`motivo`,
  `clave_idempotencia`; moves no money, logged in `ifpe.spei_descartes`).
  Pre-0004 credits that kept only the bare tracking key match with the same
  ±7-day window as everything else, so a payer who reuses a tracking key
  every week is not held forever.
- **Sending:** `POST /api/v1/spei/transfer`. The balance and the reserve go
  down at once (the money is committed to the order and cannot be spent
  twice), and the order is dispatched by a background worker. The worker
  never refunds on a timeout or an ambiguous answer. A rejection counts as
  final only on the first attempt; afterwards (for example "duplicate
  tracking key" on a resend) the order goes to manual review. A first-attempt
  rejection is applied only if, under the order's lock, `intentos_envio` still
  matches the claim (no other dispatcher took the order after the lease
  expired). Right before every `registrar` the worker re-checks its claim
  against the live row (same state, same `intentos_envio`, not in review) and
  renews the lease, and every gateway call is capped at 20 s, far below the
  60 s lease: a slow batch's stale snapshot never registers an order that
  another worker took over, that went to review or that an operator
  refunded. The worker never resends when the provider's operating day may
  have changed since the first call that might have registered the order
  (`registro_incierto_desde`; `SPEI_HORA_CORTE`, default 17:30 Mexico City,
  plus `SPEI_MARGEN_CORTE_SEG`): the provider rejects a repeated tracking key
  only within the same day. A call that provably never reached the provider
  (connection refused) does not count, so orders stuck behind a provider
  outage are sent once it recovers, even after the cutoff. Status queries
  cover every operating date any `registrar` call could have been given. A
  second accepted registration of the same order (another provider id or
  operating date) is logged as an incident and flagged for review. Refunds happen
  only on a first-attempt rejection, a cancellation or a return, exactly once
  (`spei-reverso:<order>`), even when the status webhook, the poller and an
  incoming return all report it. Settled orders are polled for returns during
  `SPEI_VENTANA_DEVOLUCION_DIAS`. An order in manual review that the provider
  never received is refunded by an operator:
  `POST /api/v1/operaciones/spei/enviados/{id}/reembolso` with `motivo` and
  `clave_idempotencia`. It re-queries the provider (it must not have the
  order), requires the dispatcher to have left the order alone for
  `SPEI_ESPERA_RESOLUCION_SEG` (default 900), and is logged in
  `ifpe.spei_resoluciones`, bound by the database to the reversal of an order
  the provider never acknowledged.
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
  account, book reserve plus orders not yet confirmed as settled, orders in
  manual review (settled ones included) and orders with a reconciliation
  incident. A provider result that contradicts an order's final state (for
  example "settled" for an order already refunded) moves no money and is kept
  in the append-only `ifpe.spei_incidentes`.
- **Corrections:** `POST /api/v1/operaciones/spei/recibidos/{id}/correccion`
  (operator) reverses a wrong credit, up to the credited amount.
  `POST /api/v1/operaciones/spei/recibidos/{id}/anulacion` (operator, `motivo`,
  `clave_idempotencia`) voids a wrong manual entry (wrong CLABE or amount): it
  corrects whatever is left of the credit and frees the payment identity, so
  the real payment (webhook or a new entry) is credited to the right account
  as its replacement (`reemplaza_a`). Provider-reported credits cannot be
  voided.

`backend/src/spei/pasarela.rs` defines the gateway interface. The simulated
gateway reproduces STP's behaviour, including duplicate rejections on resend
and delayed status queries. A production STP client (RSA-signed orders,
participant catalog) plugs in there.

## Digital onboarding and biometric KYC (migration `0006`)

Remote identification of individuals (CNBV / Ley Fintech, AML/CFT): personal
data, INE (front and back) and a liveness selfie matched against the
credential photograph.

- **Strict gating, enforced by PostgreSQL row-level security.** A client
  whose KYC is not `VERIFICADO` cannot receive deposits (SPEI or simulated),
  transfer, withdraw or invest in a mutuum. Policies on `ifpe.transacciones`,
  `sofom.contratos_mutuo` and `sofom.creditos` evaluate the client's current
  state on every insert, so verifying or revoking a client changes what it can
  do in the same transaction. This holds even if the backend skips its own
  checks. The only exceptions are not client operations: the refund of the
  client's own failed SPEI order, an operator correction, the maturity of a
  signed mutuum, and compliance holds.
- **One source of truth:** `kyc.kyc_perfiles.estado_kyc` (`PENDIENTE`,
  `EN_REVISION`, `VERIFICADO`, `RECHAZADO`). `kyc.clientes.estatus_kyc` is a
  mirror maintained by the database; no application role can write it.
- **Who can decide.** `sc_kyc` (client routes) can only edit a `PENDIENTE`
  file and send it to review; RLS makes every other file invisible to its
  updates. Only `sc_kyc_verificador` (the provider's signed result and
  operators) can verify, reject or reopen. Identity fields freeze once
  submitted, and each submission gets a database-generated verification id.
- **Evidence the database demands:** `VERIFICADO` requires a provider result
  for the *current* submission with a passed liveness check, a valid document,
  OCR whose CURP and voter key equal the file's, and a similarity score of at
  least `kyc.parametros.umbral_score_minimo`. For an automatic approval the
  result must come from the same transaction. For a manual approval it must
  reach `umbral_revision`, and the operator is recorded and cannot be the
  client. Thresholds and attempts live in `kyc.parametros`
  (`KYC_SCORE_MINIMO`, `KYC_SCORE_REVISION`, `KYC_MAX_INTENTOS` set them at
  startup).
- **Validation:** CURP (structure, date, state, RENAPO check digit, and
  letters vs the name as a warning), RFC (SAT check digit, same date as the
  CURP), INE voter key (birth date and sex vs the CURP), CIC, credential
  validity, and the ICAO 9303 TD1 MRZ check digits, cross-checked against the
  file. Warnings send the case to human review instead of auto-approval.
- **Audit:** `kyc.kyc_auditoria_biometrica` (immutable) keeps the score,
  liveness, document and OCR checks, applied threshold, origin IP and user
  agent, the signed raw result, and SHA-256 fingerprints of the images. The
  images themselves are not stored. `kyc.kyc_historial` records every
  transition with its role and operator. Express consent to biometric
  processing (LFPDPPP sensitive data) is required and kept with the
  submission.
- **Provider:** `backend/src/kyc/biometria.rs` defines the interface. Results
  are HMAC-signed JSON (`X-Kyc-Firma`, same scheme as SPEI) and always go
  through the same path, whether the provider answers immediately or later on
  `POST /api/v1/kyc/webhook`. Replays are idempotent, and a result for an
  outdated submission is refused. The score is read from the exact JSON text,
  never through a float.
- **Simulated provider = demo only (fails closed).** The bundled
  `ProveedorSimulado` only checks that the camera frames differ, and its "OCR"
  echoes the declared identity. It is installed only with `MODO_DEMO=true`.
  Otherwise `/kyc/verificar` answers 503 `KYC_PROVEEDOR_NO_DISPONIBLE` without
  using an attempt, and the database refuses results from `simulado` unless
  `kyc.parametros.permite_proveedor_simulado` is set (startup sets it from
  `MODO_DEMO`). A certified provider (passive/active liveness, 1:1 face match,
  INE nominal-list and RENAPO validation) plugs in behind the same trait.
- **Attempts are fair and bounded** (migration `0007`):
  - A provider failure, or a request cut off before the result is recorded,
    gives the attempt back.
  - An operator who asks for a new submission always leaves at least one
    attempt.
  - A CURP with 3 rejected results in *other* accounts in 30 days is blocked,
    so attackers cannot cycle accounts against one identity.
  - Probing whether a CURP belongs to a customer (`409 IDENTIDAD_EN_USO`)
    counts against a per-account limit.
  - `GET /kyc/expediente` reports these locks (`bloqueo`: `SIN_INTENTOS`,
    `CONFLICTOS_IDENTIDAD`, `IDENTIDAD_BLOQUEADA`, with
    `intentos_restantes = 0`) and the account status (`estatus_cliente`,
    e.g. `SUSPENDIDO`), so the client is never offered an attempt the
    database will refuse.
  - Clients never see the exact similarity score or the threshold, not even
    in the text of the findings; operators do, and an operator's verdict
    must name the submission (`verificacion_id`) they reviewed.
  - `coincide_identidad` in the audit log means the INE reading matches the
    declared identity (and the provider analyzed the images sent); the face
    match, liveness and document checks have their own columns.
  - `/kyc/verificar` runs at most 8 at a time, and cheap checks run before
    any image is decoded.
- **Identity squatting:** CURP, RFC and voter key are unique only among
  submitted or verified files. A value merely typed at sign-up blocks no one.
- **No side doors** (`0007`): only the bridge role (`sc_puente`) writes
  `TRASPASO_A_SOFOM` / `RETORNO_DE_SOFOM`. Every `RETORNO_DE_SOFOM` must be
  the maturity of a real contract: its SOFOM `RETORNO_A_IFPE`, same amount and
  transaction, back to the account that funded that contract. Mutuum
  contracts belong to the owner of both their accounts, and SOFOM accounts
  never change owner. A loan's borrower is fixed, and disbursing requires
  their KYC to be valid. A transfer to an account that cannot receive returns
  a generic `DESTINO_NO_ACREDITABLE`, so another customer's KYC or
  blocked-list status is never revealed.

## AML/CFT monitoring (PLD/FT, migration `0009`)

Transaction monitoring for the Disposiciones de PLD/FT, in its own `aml`
schema. **The database is the only scorer:** `aml.evaluar_operacion` computes
the score from the ledger itself, so a client or a buggy route cannot pass a
verdict. Rust only passes the facts of the operation it is about to write.

- **Every money path is evaluated before it writes**, per client and across
  every channel: incoming SPEI (webhook, operator capture, demo), internal
  transfers in and out, outgoing SPEI, mutuum investment, credit disbursement
  and repayment. A per-client lock (the IFPE account row, then an advisory
  lock) is taken before any window query, so concurrent deposits see each
  other. A ledger row that somehow arrives without an evaluation gets a
  `DIFERIDA` marker and a `COBERTURA` notification; the worker scores it.
  The facts an evaluation declares are checked at COMMIT against its ledger
  row (type and direction, channel, owner, amount and key; holds, releases,
  reversals and corrections admit no evaluation), and each transactional role
  may only evaluate its own channels (`sc_spei` incoming SPEI, `sc_ifpe`
  transfers and outgoing SPEI, `sc_puente` the mutuum): a role cannot forge
  an alert, a freeze or a blocked attempt for a channel it does not operate.
- **Rules** live in `aml.reglas_riesgo` (versioned, rows immutable) and the
  thresholds in `aml.parametros`. Inline (windows up to 24 h): R01 relevant
  operation (quarterly register, not an alert), R02 fragmentation (fires again
  on every further sub-threshold credit once the 24 h sum reaches the
  threshold), R03 high-risk client and R04 out of profile (one alert per
  client, direction and day; a false positive silences them for the rest of
  the day), R05 structuring (in-band credits; the ALTO band is 25% wide with
  frequency 3, so two ordinary credits of a PEP do not freeze), R12 frequent
  structuring (at least 5 credits in the wide band [50%, 100%) of the
  threshold summing at least 600,000: catches amounts kept one peso below the
  R05 band), R07 daily volume, R08 frequency, R11 rapid pass-through (only
  money that credited the vault counts as inflow, not a loan repayment; only
  outflows to counterparties that each take at least 10% of the inflow count,
  so a dispersed payroll is not pass-through); modifiers for PEP, high-risk
  geography, risk tier and new accounts; R09 Blocked Persons List. Batch worker: R20 slow
  structuring (7 and 30 days), R21 monthly volume vs declared income, R22/R23
  fan-in and fan-out, R24 48-hour pass-through, R25 circular flows between
  clients, R26/R27 one payer or one beneficiary shared by several clients.
  Score = primary points + modifiers (capped at 100): <30 BAJO, 30–59 MEDIO,
  60–84 ALTO, ≥85 CRITICO. Every rule allowed to freeze (R05 90, R11, R12 and
  R20 85) reaches CRITICO on its own, so an aged BAJO or MEDIO account is
  frozen like a new one. Changing a rule or a parameter (including the
  observation-mode kill switch) is a proposal that a second officer approves;
  a deactivated rule can be re-activated the same way. Proposals and unfreeze
  requests can only be created `PENDIENTE` by an officer and decided by the
  officer acting in the decision function (a row inserted already approved is
  rejected). A same-day sale followed by a purchase that takes most of the
  proceeds to one counterparty is indistinguishable from pass-through and is
  frozen for review; a purchase paid from earlier savings is not.
- **Freeze semantics.** An automatic freeze needs a CRITICO score, a fired
  rule allowed to freeze, and observation mode off; profile factors alone
  never freeze. A freeze is per client: the IFPE account goes to
  `CONGELADO_PREVENTIVO`, mirrored one-to-one by an `ACTIVO` row in
  `aml.congelamientos` (checked at COMMIT). A database guard then rejects
  every outgoing movement (transfer, SPEI, mutuum, credit disbursement) even
  if the backend skipped its checks. A blocked attempt commits the
  evaluation, the alert and the freeze **without any ledger row**. Incoming
  money is never refused: it is credited and then held (`RETENCION`, so the
  100% reserve and the shadow balances are unchanged), covering the pattern
  funds; every later credit to a frozen client is held in full. Refunds,
  maturities and operator corrections still work; a correction is recognized
  by its `ifpe.spei_correcciones` record, not by its key, and a SPEI order can
  never use a reserved ledger key (`spei:`, `spei-reverso:`,
  `spei-correccion:`, `ret:`, `lib:`). SPEI orders of a frozen client that the
  provider provably does not have (pending, no `registrar` call that could have
  registered them, not in manual review) are not dispatched, including an
  order already claimed whose call never reached the provider and a freeze
  that commits between the claim and the first `registrar` call.
- **Four-eyes.** One officer may tighten (take, escalate, confirm, freeze).
  Unfreezing, releasing holds and approving a false positive while the client
  is frozen need a second officer, enforced in the database
  (`SC_AML_CUATRO_OJOS`), as is acting on one's own client record
  (`SC_AML_AUTORESOLUCION`). An unfreeze either releases the holds or keeps
  them for a later, separate release request. A false-positive proposal is
  bound to the unfreeze request it was made under: only approving that request
  resolves it (a hold release on an older freeze does not), and rejecting it
  withdraws only its own proposals. New evidence merged into a proposed alert
  withdraws the proposal (`ALERTA_FP_RETIRADO`), and no request is approved
  while a proposed alert has evidence newer than its proposal. A false
  positive sets a suppression watermark per rule and direction, so the
  reviewed movements do not re-freeze the client, while unreviewed movements
  in the other direction still count.
- **No tipping off.** A frozen client gets one constant error for every
  blocked operation (`422 OPERACION_NO_DISPONIBLE`, the same bytes whatever
  the reason), keeps seeing `"estado": "ACTIVA"`, and sees a hold only as a
  neutral "Retención temporal". `/yo`, `/tablero` and `/auth/registro` no
  longer return the risk tier or PEP flag. Freezing, lifting and CONFIRMAR
  change nothing a client can read: the account's and the client's
  `actualizado_en` keep their value when only the AML state or the risk tier
  changes (a ledger movement still moves them). Operator holds
  (`POST /operaciones/retenciones`) look exactly like AML holds in the
  client's ledger: the same `ret:<id>` / `lib:<id>` keys and neutral concepts,
  with the operator's reason and key kept in `ifpe.retenciones_operador`
  (before, an operator hold carried `op:<operator>:<key>` and the operator's
  text, so a `ret:` hold could only be AML). Senders to a frozen client see a
  normal success. Client roles have no access to the `aml` schema at all,
  including `sc_auditor`, which serves `/tablero` and `/reserva/prueba`.
- **Risk tier.** `kyc.clientes.nivel_riesgo_pld` feeds the score (R03, the
  per-tier bands, M03) and CONFIRMAR raises it to ALTO. The operator's KYC
  dictamen (`POST /operaciones/kyc/{id}`) still assigns it at onboarding, but
  it cannot lower it while the client is under AML review (an open or
  confirmed alert, or an active freeze): the database keeps the tier without
  an error, so an operator cannot probe for a case, and logs
  `RIESGO_CLIENTE_REDUCCION_DENEGADA`. Every tier change made outside the
  compliance functions is logged in `aml.bitacora` with the operator (or the
  database user) as actor. The dictamen answers with the client projection
  (no tier, evaluation time or PEP flag), and an empty dictamen is a 400.
  Lowering the tier of a confirmed client is a schema-owner action, and it is
  logged too.
- **Deadlines and audit.** Alerts carry the UIF reporting deadline (60 days,
  24 h when urgent). `aml.bitacora` is append-only with a canonical hash per
  row, sealed by the worker in chained batches by transaction id (each seal
  covers only transactions that had already finished, so a late commit never
  looks like tampering). The worker records the last verified seal in
  `aml.verificaciones_bitacora`; `GET /api/v1/aml/bitacora/verificacion`
  re-verifies from there (rows, seal chain and the unsealed tail), which stays
  within the request limits however long the log grows; a failed verification
  is kept forever. A full verification is `aml.verificar_bitacora(1)`, run by
  the schema owner or by a login granted `sc_auditor_pld` (a role that can
  run that function and nothing else in `aml`). Every case-file view is
  logged.
- **Batch worker** (`AML_MONITOREO_INTERVALO_SEG`, default 300 s, 0 disables;
  `AML_MONITOREO_LOTE` ledger rows per cycle): one instance at a time (a
  lease of at most 600 s in `aml.monitoreo_estado`, fenced by a token: only
  the holder renews it, records the cycle's range, advances the watermark or
  releases it; it is renewed between steps). It scans the clients that moved
  since its ledger watermark with the batch rules evaluated at the times of
  their movements in the range (a lagging worker sees what a punctual one
  would; a range starting more than 24 h ago raises a `COBERTURA` notice), and
  the network rules. The watermark only advances up to the range the cycle
  recorded, never past the ledger heads. A batch alert is not duplicated while
  open; after it is closed, a pattern with evidence newer than the closure
  opens a new alert (and may freeze). The network rules apply each client's
  false-positive watermark. Reading a client's 30 days takes no lock; the
  client lock (without waiting) only covers creating alerts and freezing, so
  a long sweep never times out that client's money paths. A client whose sweep
  fails three cycles running is quarantined (`COBERTURA` notice), stops holding
  back the watermark and is retried separately. The worker also re-scores
  `DIFERIDA` markers, raises SLA and UIF-deadline notifications on the server
  clock (a caller-supplied instant is honored only for the schema owner),
  seals and verifies the audit log and delivers the notification outbox to the
  log (`target: "aml"`, codes and folios only). Nothing uses `pg_notify`.
- **Roles.** `CUMPLIMIENTO` (compliance officer) has the console;
  `AUDITOR` reads it; `OPERADOR` and `CLIENTE` get 403 on every console
  route, checked before the path, query or body is read (a malformed request
  still gets 403, so the request schema cannot be explored). The database
  repeats the role check in every function. Nobody sees a case about
  themselves: an officer or auditor who is the subject of an alert (an
  `INTERNA_PREOCUPANTE` case) does not see it, its restriction, its four-eyes
  request, its notifications or their relevant operations; the summary
  counters leave them out, and the case file and client review answer 404 as
  if they did not exist. Officer POSTs take a `clave_idempotencia` (201 when
  performed, 200 on a replay, 409 when the key was used for something else).
- **Console.** The feed polls by a change marker that also covers what each
  row embeds: a change of the linked restriction (lift, confirmation, SLA) or
  of the client (risk tier, PEP flag, name) moves the alert's `actualizado_en`,
  so every open console drops the "Client restricted" lock after a lift. An
  action refused because the case changed meanwhile (409, e.g. another officer
  took the alert) reloads the case file with the conflict message; a 401 ends
  on the sign-in page with the "session expired" notice; a deep link with a
  rule code or alert id the API rejects is ignored ("Alert not found") instead
  of being retried forever. Internal users are named after their role and the
  local part of their email ("Cumplimiento oficial1"), so the requester,
  proposer and approver of a four-eyes decision are distinguishable.

| Method | Route (`/api/v1/aml/...`) | Access |
|---|---|---|
| GET | `resumen`, `alertas` (filters, cursor, change polling) | officer, auditor |
| GET | `alertas/{id}` (case file), `clientes/{id}` (client review) | officer, auditor (logged) |
| POST | `alertas/{id}/resolver` (TOMAR, LIBERAR, ESCALAR, CONFIRMAR, FALSO_POSITIVO) | officer |
| POST | `alertas/{id}/reporte-uif` (UIF/SITI folio, once) | officer |
| POST | `clientes/{id}/congelar` (manual freeze) | officer |
| GET | `congelamientos`, `solicitudes`, `notificaciones`, `relevantes` | officer, auditor |
| POST | `congelamientos/{id}/solicitudes` (unfreeze or release request) | officer |
| POST | `solicitudes/{id}/decision` | a second officer |
| GET | `reglas`, `propuestas` | officer, auditor |
| POST | `propuestas`, `propuestas/{id}/decision` | officer; a second officer decides |
| GET | `bitacora/verificacion` | officer, auditor |

The legacy `kyc.alertas_pld` is now read-only history: open alerts were copied
into `aml.alertas` and `GET /api/v1/operaciones/alertas` was removed.

**Residual risks (accepted, with the reasons).**

- *A sender can infer a hold on the recipient from the ledger position.* A
  hold on an internal transfer to a frozen client (or on a credit that scores
  CRITICO) is written in the sender's own transaction, so it takes the
  ledger sequence right after the sender's TRANSFERENCIA. The live ledger
  head is public (`GET /api/v1/reserva/estado`), every published snapshot
  anchors (`secuencia_ledger`, `hash_ledger`), and clients see `secuencia`
  and `hash_anterior` of their own rows, so on a quiet ledger a sender can
  tell that another row followed their transfer in the same transaction.
  It is not fixed because both ways out weaken an invariant: writing the hold
  in a later transaction would leave the credit unheld between the two commits
  (the deferred `SC_AML_RETENCION_FALTANTE` check exists to forbid exactly
  that), and hiding positions from clients does not help while the head is
  public for Proof of Reserve (per-account chains would be a new ledger
  design). Concurrent traffic adds noise, the gap does not say why the row
  is there, and repeated probing transfers count towards R08 (frequency).
- *The database four-eyes check trusts the officer id the backend passes.*
  All officers share the `sc_aml` database role, so the officer functions take
  the acting officer as a parameter: the database checks that the id belongs
  to a `CUMPLIMIENTO` user, is not the subject and differs from the first
  officer, but it cannot tell whether the session really is that officer.
  The backend binds the id from the verified JWT (checked against the
  database on every request) and `sc_aml` is granted only to the application
  user, so abusing this needs code execution as the application or its
  database credentials, which can already `SET ROLE` to every `sc_*` role.
  A real fix needs per-officer database identities (or single-use tokens
  issued at login and consumed by the officer functions). Every bitácora row
  records the database user and role next to the officer.
- *Hold timing.* Operator and AML holds now have the same shape, but an AML
  hold still lands in the same transaction (and instant) as the credit it
  holds, for the credited or pattern amount, while an operator hold stands
  alone. A client who studies their ledger can still guess that a hold that
  arrives with a credit came from monitoring. Seeing funds on hold is an
  accepted residual (spec §7); the hold cannot move to a later transaction
  for the reason above.
- *Staff seen as counterparties.* The subject filter covers records whose
  client is the officer. An officer who only appears in another client's
  alert (as a transfer counterparty or as a member of an R25-R27 network) can
  still open that alert.

## Authentication and two-step verification (migration `0010`)

- **Who.** Two-step verification (TOTP, RFC 6238: SHA-1, 6 digits, 30 s) is
  **mandatory for every staff role**: `OPERADOR` (the back-office
  administrator), `AUDITOR` and `CUMPLIMIENTO`. Clients may turn it on.
- **Sign-in.** `POST /api/v1/auth/login` answers `estado`:
  - `OK`: a session (clients without two-step verification);
  - `2FA_REQUIRED`: the password was right; `token_mfa` (5 minutes) and the
    code go to `POST /api/v1/auth/verify-mfa`;
  - `MFA_ENROLLMENT_REQUIRED`: staff without two-step verification;
    `token_inscripcion` (15 minutes) calls `POST /api/v1/auth/mfa/setup`
    (secret, `otpauth://` URL and its QR code as an SVG) and
    `POST /api/v1/auth/mfa/enable` with the first code, which starts the
    session.

  A password alone never gives staff a session.
- **Enforcement.**
  - Access tokens carry `mfa`, and every request re-checks the database (role,
    revocation, two-step verification still on).
  - Staff without a verified second step get `403 MFA_REQUERIDO` on every
    endpoint, and the AML console checks it again at its boundary.
  - The partial tokens have their own audiences and a key derived from
    `JWT_SECRET`, so they never pass as access tokens.
- **Codes.** A code is accepted within ±1 step and never twice
  (`totp_ultimo_paso`). Wrong codes count with wrong passwords: the fifth
  failure locks the account for 15 minutes. A correct password does not reset
  the counter while the code is missing, so repeating it buys no extra guesses.
- **Secrets at rest.**
  - The TOTP secret is encrypted by the backend (AES-256-GCM, key
    `MFA_CLAVE_CIFRADO`, kept outside the database). The associated data binds
    the ciphertext to its owner.
  - The database rejects any other format, so a cleartext secret does not fit,
    and stores password-reset tokens only as SHA-256 hashes.
  - Only the `sc_auth` role reads these columns; `sc_kyc` lost its table-wide
    `SELECT`.
  - Once two-step verification is on, only the schema owner can turn it off or
    change the secret (trigger). That change, like every enrollment, code and
    reset, is recorded in `kyc.eventos_autenticacion` (immutable).
- **Lost device.** The DBA, as the schema owner (`scripts/db-client.sh` on a
  single host, `scripts/db-tunnel.sh --psql` on AWS), runs:

  ```sql
  UPDATE kyc.credenciales
     SET mfa_enabled = FALSE, totp_secret = NULL, totp_ultimo_paso = NULL, tokens_validos_desde = now()
   WHERE cliente_id = '<id>';
  ```

  The person enrolls again at the next sign-in.
- **Password reset.**
  - `POST /api/v1/auth/password/forgot` always answers 202 with the same body.
  - The emailed link carries a 256-bit token in its URL fragment
    (`/auth/reset-password#token=...`), so it never reaches server logs.
  - The token expires in 30 minutes and works once. Requests less than a minute
    apart send no new email.
  - `POST /api/v1/auth/password/reset` sets the new password, unlocks the
    account and **revokes every session**. Two-step verification stays on.
  - Email is simulated: with `MODO_DEMO` the link is logged (`target:
    "correo"`); otherwise nothing is sent or logged, and production needs an
    email provider (DEPLOYMENT.md).
- **Sessions.** Staff sessions last `JWT_TTL_PERSONAL_MINUTOS` (default 30).
  Turning on two-step verification or resetting the password revokes every
  other session.
- **Key rotation.** Set the old key as `MFA_CLAVE_CIFRADO_ANTERIOR` and the new
  one as `MFA_CLAVE_CIFRADO`, then run `synthetic-capital migrar`: it
  re-encrypts every secret with the new key. Then remove the old one.

## Request hardening, rate limits and row-level security (migration `0011`)

- **No mass assignment.** Every type a handler reads with `Json<T>` or
  `Query<T>`, and every struct nested in one, has
  `#[serde(deny_unknown_fields)]`. A field the API does not define (`"rol"` in
  a sign-up, `"comision"` in a transfer, `?cliente_id=` on a list) is rejected
  with `400 VALIDACION` (`unknown field ...`) instead of being ignored.
  - `tests/entrada.rs` scans the source: a new request type without the
    attribute, or an open `serde_json::Value` field not on its reviewed list,
    fails the build. One field is open on purpose, `SolicitudPropuesta.cambios`:
    the database validates each key.
  - The provider webhooks are the exception: they read the raw body, check the
    HMAC first and tolerate fields a provider adds.
- **Rate limits per client IP** (`routes/limites.rs`, `tower_governor`): the
  same limits as the nginx edge, enforced again in the service. Over the limit
  the answer is `429 REINTENTAR` with `Retry-After`, exactly as the edge sends.

  | Group | Routes | Default (`HTTP_LIMITE_<GROUP>_POR_MINUTO`, `_RAFAGA`) |
  |---|---|---|
  | `AUTH` | `/api/v1/auth/*` (sign-up, sign-in, two-step verification, password reset) | 5 per minute, burst 10 |
  | `KYC` | `POST /api/v1/kyc/verificar` | 4 per minute, burst 4 |
  | `API` | the rest of `/api/v1`; auth and KYC requests count here too | 600 per minute, burst 50 |
  | `WEBHOOK` | `/api/v1/spei/webhook/*`, `/api/v1/kyc/webhook` (outside the API bucket) | 3000 per minute, burst 200 |

  - The key is the client IP that `IpCliente` reads. Behind the trusted proxy
    (`HTTP_PROXY_CONFIABLE`) that is the last `X-Forwarded-For` hop, never the
    first, which a client could write to dodge the limit or use up someone
    else's.
  - IPv6 counts per /64. `/salud` and `/vivo` are never limited.
  - Each process counts in memory. With N tasks behind the load balancer an
    address can reach N times the quota; the global per-IP limit is the WAF's.
  - `HTTP_LIMITE_IP=false` turns them off, for tests and load runs from one
    address only.
- **Row-level security per client.** The reads made on a client's behalf (profile,
  IFPE account and movements, SOFOM position, SPEI orders and transfers
  received) run as the role `sc_cliente`, with the client's id in the
  transaction-local setting `sc.cliente_actual` (`db::begin_lectura_cliente`).
  - RESTRICTIVE policies on `kyc.clientes`, `ifpe.cuentas`,
    `ifpe.transacciones`, `ifpe.spei_outbound`, `ifpe.spei_inbound`,
    `sofom.cuentas`, `sofom.contratos_mutuo` and `sofom.transacciones` let it
    see only that client's rows, whatever the query asks for.
  - A transfer between clients is visible to both. Without the setting the
    role sees nothing, and a value that is not a UUID makes the query fail.
  - `sc_cliente` only has `SELECT`. Clients never write rows directly: money
    moves through the ledger roles, which check balances, reserve and AML.
    Nobody deletes movements, accounts or KYC files (immutable ledger, legal
    retention). The policies are `FOR ALL` anyway, so any write ever granted
    stays within the client's own rows.
  - Every other role works as before.
  - This guards against application bugs (a wrong id, a missing `WHERE`), not
    against SQL injection inside that transaction, which could rewrite the
    setting. Against injection, every query is parameterized.
  - Tests: `tests/rls.rs` and `tests/db/0011_rls_por_cliente.sql`.
- **Dependency audits.** Both image builds fail on a known vulnerability (see
  "Dependency audits" under "Running locally").

## Sovereign Vault: snapshot and real-time channel (migration `0012`)

The dashboard's vault (Bóveda Soberana) comes from two client-only routes in
the `API` rate-limit group. Their JSON contract is
`frontend/src/types/vault.ts`; the dashboard's home tab (`/dashboard`) renders
it, with a cash-balance chart rebuilt from the IFPE ledger.

- **`GET /api/v1/vault`** returns the whole vault in one call, read as the
  client with row-level security (`sc_cliente`, one REPEATABLE READ snapshot):
  - `user_id`, `clabe`, and `bank_name` (`SPEI_BANCO_NOMBRE`, default `STP`);
  - `balance`:
    - `available_cash`: the IFPE account's available balance;
    - `invested_assets`: the principal of the client's SOFOM mutuums that are
      in force or matured and not yet settled, plus the market value of their
      positions in the market (migration `0013`);
    - `total_mxn`: available plus held plus invested;
    - `unrealized_pnl_percentage`: (market value − cost of the positions) /
      (mutuums + cost of the positions), in percent. Mutuums are carried at
      cost and their interest is realized at settlement, so only market
      positions move it; `"0.00"` with nothing invested.
  - `reserve_proof`: the Merkle root of the last published reserve cut
    (`zk_root_hash`) and its status:
    - `verified` when that cut covers the deposits and the live reserve is at 100 %;
    - `failed` otherwise;
    - `pending`, with a root of 64 zeros, until the first cut.
  - `holdings`: the client's market positions at the current price
    (`shares`, `current_price`, `total_value`, `daily_change_pct`); `[]` with none.
  - Amounts are decimal strings with 8 decimals, like the rest of the API.
- **`GET /api/v1/vault/ws`** is a WebSocket (`backend/src/boveda/`) that
  pushes every change to the vault.
  - **Authentication.** A browser cannot set headers on a WebSocket, so the
    JWT travels as a subprotocol entry:
    `new WebSocket(url, ["sc.vault.v1", "bearer." + token])`.
    - The server confirms only `sc.vault.v1`; the token is never echoed back.
    - Non-browser clients may send `Authorization: Bearer` instead, but must
      still offer `sc.vault.v1`.
    - The checks are those of every authenticated route: signature, audience,
      expiry, and then current role, revocation, block and second factor in
      the database.
    - `Origin`, when present, must equal `FRONTEND_ORIGIN`, because CORS does
      not cover WebSockets.
    - The edge's CSP (`connect-src 'self'`) already allows `wss:` to the
      same host.
  - **Handshake errors** use the usual JSON envelope:

    | Status | Code | Cause |
    |---|---|---|
    | 400 | `WEBSOCKET_REQUERIDO` | not a WebSocket upgrade |
    | 400 | `PROTOCOLO_REQUERIDO` | `sc.vault.v1` not offered |
    | 401 | `NO_AUTENTICADO` | missing, invalid, expired or revoked token |
    | 403 | `PROHIBIDO` | wrong `Origin`, or not a client |
    | 429 | `DEMASIADAS_CONEXIONES` | the client already has 5 sockets open on this task |
    | 429 | `REINTENTAR` | per-IP rate limit (`API` group) |
    | 503 | `REINTENTAR` | the task already holds `HTTP_MAX_CONEXIONES` sockets |

  - **Messages.** Every message carries `seq`, the account's version.
    - **`vault.snapshot`**, the whole vault, is sent:
      - first, when the socket opens;
      - after `{"type":"resync"}` from the browser. At most one per second:
        requests within that second are merged into one;
      - when a reserve cut is published (same `seq`, new proof);
      - when the socket fell behind;
      - when the server's `LISTEN` reconnects.
    - **`spei.transfer`**: an inbound SPEI credited (`inbound`/`settled`) or an
      outgoing order refunded (`outbound`/`returned`), with the balance it left.
      `counterparty_bank` is the 3-digit bank code of the CLABE (`012` is BBVA
      México); there is no name catalog yet.
    - **Any other balance change** (internal transfer, outgoing order,
      investment, market order, hold) arrives as a `vault.snapshot`.
    - **Ordering.**
      - A delta is only sent with a `seq` above the last one sent.
      - A gap means messages were skipped. The browser marks the vault stale
        and asks for `resync`.
      - Balances are always those of the `seq` they carry. Two changes that
        commit within milliseconds can merge into one message, so the transfer
        feed is best-effort; the balance is not.
  - **Liveness and session.**
    - The server pings every 25 s. A socket silent for 75 s is closed with
      1001.
    - The session is re-checked against the database every 60 s.
    - The socket closes with **4001** when the JWT expires or the session is
      revoked or blocked; the browser signs in again.
    - A deploy closes sockets with **1001**, and the browser reconnects.
  - **Limits.**
    - Each backend task holds up to `HTTP_MAX_CONEXIONES` sockets, and 5 per
      client.
    - An open socket no longer counts against the HTTP connection limit.
    - Vault reads (snapshots and notified changes) use at most half of the
      database pool at a time, so a burst waits instead of starving HTTP
      requests.
    - The browser may send 4 KB per message.
- **The dashboard's side** (`frontend/src/lib/vault-socket.ts`, run by
  `useVaultSocket` in the dashboard shell):
  - **One socket** per signed-in browser tab, kept across the dashboard's tabs.
  - **Messages:** every frame goes to the store's `receive()`, which applies
    them in order. A gap, or a delta before any snapshot, sends `resync`.
  - **Reconnection:** after a drop or a deploy (1001), with jittered backoff
    from 1 s to 30 s. It reconnects at once when the browser comes back online
    or the tab becomes visible.
  - **Session end:** a 4001 signs out to the sign-in page instead of
    reconnecting.
  - **REST:** `GET /api/v1/vault` gives the first paint and fills in while the
    stream is down; its polling pauses while the stream is live.
  - **Other tabs:** a newer version reloads the summary and the ledger, so the
    chart and the Account tab follow.
  - **Header indicator:** reads "Live" only once a snapshot has applied.
- **Where versions come from (migration `0012`).**
  - `ifpe.cuentas.seq` goes up by exactly one in every transaction that
    changes an account's balances. A trigger does it, so every code path is
    covered and nobody can set or rewrite it.
  - On commit, PostgreSQL sends `NOTIFY sc_boveda`. Notifications are
    delivered only for committed transactions, in commit order.
  - The SPEI credit or refund that moved the money carries the same
    transaction id (`xact`), which is how a change is matched to its transfer.
  - Each backend task holds one connection from its pool for
    `LISTEN sc_boveda`. It probes that connection every 30 s, reconnects when
    it fails, and fans changes out to its own sockets, so any number of tasks
    work behind the load balancer without coordination.
  - Tests:
    - `tests/db/0012_boveda_tiempo_real.sql`: the triggers, in one
      transaction.
    - `backend/tests/boveda.rs`: delivery, the REST contract, and the
      WebSocket end to end against the real server (handshake rules, caps,
      rate limit, heartbeat, revocation, LISTEN reconnection, shutdown).

## Markets: catalog, quotes and market orders (migrations `0013`, `0014`)

The trading screens (`frontend/src/components/markets/`) read one contract,
`frontend/src/types/market.ts`. The backend is `backend/src/mercado/`, with
three routes in the `API` rate-limit group, all behind the JWT extractor:

- **`GET /api/v1/market/assets`** (any signed-in session): the catalog for
  the Discover tab, by ticker. Each entry is the quote's `MarketAsset` plus
  its `price_source`, read from `mercado.activos`: the table quotes and orders
  use, so every listed asset can be opened and traded. Outside `MODO_DEMO`
  only live prices are listed, and the answer is 503 `MERCADO_NO_DISPONIBLE`
  when none is.
- **`GET /api/v1/market/quote/{ticker}`** (any signed-in session): the asset
  (`MarketAsset`: price, 24-hour change against the previous close, market
  cap, volume, yield, quantity increment) and its price history for the
  chart: every 15 minutes over the last day, hourly over the last week, daily
  for a year. Unknown tickers are 404.
- **`POST /api/v1/market/order`** (clients):
  `{"ticker", "side": "buy" | "sell", "amount": "1250.00", "idempotency_key"}`.
  A market order fills at once at the current price and answers an
  `OrderReceipt` (201; 200 with the same receipt when the key repeats; 409
  when the key was used for something else).
  - **Buy:** as many units as the amount covers in the asset's increment
    (0.0001 g of gold, one whole bill). The charge is `quantity × price` to
    the cent, never more than asked; the rest stays as cash. 1,250 MXN of
    gold at 1,612.40 buys 0.7752 g for 1,249.93.
  - **Sell:** the amount in pesos to sell. An amount worth the whole position
    (the checkout's Max) sells all of it, with no dust; more than the
    position is worth is 422 `POSICION_INSUFICIENTE`. A sale releases the
    position's cost at average cost.
  - **Commission:** zero, and a database constraint keeps it so.
  - **Errors:** 422 `SALDO_INSUFICIENTE`, `MONTO_INSUFICIENTE` (less than one
    increment), `POSICION_INSUFICIENTE`, `MERCADO_CERRADO`,
    `KYC_NO_VERIFICADO`, `OPERACION_NO_DISPONIBLE` (AML freeze, the constant
    message); 400 for fractions of a cent, numbers instead of strings or
    unknown fields.
- **Money path.** One transaction under the role `sc_mercado`, in the global
  lock order: the client's IFPE account, the AML lock (buys), the custody
  reserve, then the ledger heads, the asset (`FOR SHARE`) and the position.
  - The cash leg is an IFPE ledger movement: `COMPRA_MERCADO` takes cash out
    of the vault and the custody reserve (like an investment in a mutuum);
    `VENTA_MERCADO` brings it back. The 100 % reserve, the shadow balances
    and the hash chain hold as for every other movement, and the replay a
    Proof of Reserve cut runs before publishing (`reserva::saldos_sin_ledger`)
    counts both. The integration tests run that same replay after every money
    path, so a new movement type it does not know fails there first.
  - The order itself is a row in `mercado.ordenes`, the market's own
    append-only ledger, hash-chained like the IFPE and SOFOM ledgers
    (`SC-MERCADO-v1`). Each movement has its order in the same transaction,
    and each order its movement.
  - Before inserting, the database checks the order against the catalog and
    the position:
    - the current price and source, and a quantity in the asset's increment;
    - for a buy, the largest quantity the amount covers;
    - an amount equal to `quantity × price` to the cent;
    - for a sale, a position that covers it and the exact average cost it
      releases.
  - `mercado.posiciones` is written only by the order trigger, and on commit
    it must equal the replay of its ledger (`SC_POSICION_SIN_LEDGER`).
  - The balance change raises the vault's `seq`, so the WebSocket pushes the
    new vault (cash, holdings, invested, P&L) when the order commits.
- **KYC and AML.** Only clients with verified KYC trade (RLS on the order and
  on the IFPE movement).
  - A buy is a new AML channel, `COMPRA_MERCADO` (outgoing). It works like
    an investment in a mutuum: online evaluation before any write, the
    freeze guard, coverage and velocity. It does not count toward
    structuring or pass-through.
  - A sale is the client's own money coming back, like a matured mutuum: it
    is not evaluated.
  - A frozen client neither buys nor sells. A blocked buy still records the
    attempt.
- **Simulated prices.** Until the pricing oracle is connected, the catalog
  has `SIMULADO` prices and the history is generated:
  - `AUG1` tokenized gold and `AGT1` tokenized silver (0.0001 increments);
  - `SCT28`, a 28-day synthetic bill, and `CETES28`, CETES at 28 days (whole
    titles; `0014`);
  - `STSLA`, a synthetic Tesla share (0.0001 increments; `0014`).

  The history is deterministic, decimal-only, ends at the current price and
  passes through the previous close exactly 24 hours earlier. Outside
  `MODO_DEMO`, a simulated price is neither listed, quoted nor traded (503
  `MERCADO_NO_DISPONIBLE`), so production never fills at a demo price. The
  oracle will update `mercado.activos` (price, previous close, source
  `ORACULO`).
- **Frontend: Discover** (`/dashboard/discover`, a tab in the sidebar and the
  phone's tab bar):
  - The catalog is validated whole (`frontend/src/lib/market-data.ts`) and kept
    in `frontend/src/store/market.ts` with the search, so coming back from an
    asset shows the same results at once while the prices refresh.
  - The search matches the ticker, the name or the class, accents and case
    aside (`frontend/src/lib/discover.ts`). With nothing typed the list is
    Trending: the most traded over the last 24 hours first.
  - Every row opens the asset's page, where a first buy happens. The Vault's
    empty asset list points here.
  - Above the list, the day's top movers (the largest 24-hour rise and fall
    in the whole catalog) and three collections (Hard Assets, Synthetic
    Equities, Fixed Income) with their assets and equal-weight 24-hour
    change (`frontend/src/components/markets/discover-highlights.tsx`). On a
    phone each is a row that swipes; both step aside while a search is typed.
  - The screener (`frontend/src/components/markets/asset-screener.tsx`)
    narrows the list by asset class and by the direction of the 24-hour
    change. Choices add up within a row and narrow across rows, combine with
    the search, and show what each would leave; a collection presets the
    asset class. All of it is computed from the catalog
    (`frontend/src/lib/screener.ts`): no other request.
- **Frontend: the asset's page** (`/dashboard/asset/[ticker]`, inside the
  dashboard shell):
  - It belongs to the tab that opened it: from Discover, Discover stays
    current and Back returns there; from the Vault's holdings (or opened by
    address), the Vault.
  - On load it reads the quote with the session's token and validates it
    whole (`frontend/src/lib/market-data.ts`) before `AssetDetail` renders it.
    An unknown ticker shows "No such asset"; a 503 says the market is not
    open yet.
  - The cash and the position come from the vault store. An order carries the
    checkout's idempotency key, kept on a retry, so a lost answer never fills
    twice. The page validates the receipt and never writes the store: the
    commit raises `seq`, and the WebSocket brings the new cash and position.
  - Each row of the vault's asset list opens its asset's page. The ledger
    shows the cash legs as "Market buy" and "Market sale", and the cash chart
    counts them.
- **Frontend: order types** (the checkout drawer,
  `frontend/src/components/markets/order-checkout-drawer.tsx`):
  - A toggle at the top chooses Market, Limit, Stop or, to buy, Savings plan.
    A limit or stop adds its price, typed on the same numpad as the amount. A
    savings plan asks how often (weekly, every 2 weeks, monthly) and shows
    what that adds up to in a month.
  - Each type has its own estimate and rules (`assessOrder` in
    `frontend/src/lib/market.ts`):
    - a limit estimates at its price, and says when the price already meets it;
    - a stop must be above the price to buy and below it to sell;
    - a sale can take what the position is worth at the order's price;
    - an amount too small for one unit is refused before it is sent.
  - The contract is `OrderRequest` in `frontend/src/types/market.ts`, a union
    on `order_type` with `limit_price`, `stop_price` and `frequency`. Market,
    limit and stop orders are live (`LIVE_ORDER_TYPES`; next section); a
    savings plan can be set up and previewed, but its button stays disabled
    with a note saying why. `api.placeOrder` sends a market order without
    `order_type` and a limit or stop order with its type and price.
- **Audit:** `GET /api/v1/auditoria/ledger/mercado` (operator / auditor)
  re-verifies the chain, replays every position and every sale's average
  cost, and checks each order against its IFPE movement.
- Tests:
  - `tests/db/0013_mercado_valores.sql`: the database rules, in one
    transaction.
  - `backend/tests/mercado.rs`: the API (catalog, quote, buy and sell,
    idempotency, rules, KYC, roles, AML freeze, concurrent orders, tamper
    detection).
  - `backend/tests/boveda.rs`: an order pushed over the WebSocket.
  - `frontend/e2e/smoke.mjs`: Discover and the trading loop in the browser.
    The catalog and its search; a first buy from Discover, sold back from the
    Vault; a buy on another device appears live; a buy whose answer is lost
    fills once; refusals show their reason; "Max" sells everything on a
    phone; the ledger lists the legs; a new reserve cut still succeeds.

## Limit and stop orders (migration `0015`)

A limit or stop order waits for its price with what it would use **held**,
so nothing spends it twice, and the trigger engine fills it at the current
price when the price reaches it. The backend is
`backend/src/mercado/pendiente.rs`; the contract, `PendingOrder` in
`frontend/src/types/market.ts`.

- **`POST /api/v1/market/order`** takes `order_type` (`market` when absent,
  the original contract) and the order's price:
  `{"ticker", "side", "amount", "idempotency_key", "order_type": "limit", "limit_price": "1600.0000"}`
  or `"order_type": "stop", "stop_price"`. A market order answers its
  `OrderReceipt` (now with `"order_type": "market"`); a limit or stop order
  answers a `PendingOrder`: its terms, `status` (`pending`, `filled`,
  `cancelled`, `rejected`), `held_cash` / `held_quantity`, and its
  `execution` once filled. 201 for a new order; 200 when the key repeats the
  same order (as it stands now); 409 `CLAVE_IDEMPOTENCIA_REUTILIZADA` when
  the key was used for another order, market or pending.
  `"order_type": "savings_plan"` is 422 `TIPO_ORDEN_NO_DISPONIBLE` until the
  plans' scheduler exists.
- **`GET /api/v1/market/orders/pending`** (client): `{"orders": [...]}`, the
  client's orders still waiting, newest first.
- **`POST /api/v1/market/orders/{id}/cancel`** (client): the cancelled order,
  its cash or units released. Repeating it answers the same order; one that
  already filled or was rejected is 409 `ORDEN_NO_PENDIENTE`; another
  client's order does not exist (404). Cancelling works on a frozen account:
  giving back what is held is always allowed.
- **What a pending order holds:**
  - A **buy** holds its whole amount: a third balance of the IFPE account,
    `saldo_comprometido`, moved from `saldo_disponible` by a new
    hash-chained IFPE movement, `COMPROMISO_ORDEN`, and back by
    `LIBERACION_ORDEN` when the order closes. Both stay inside the account
    and never touch the custody reserve: the liabilities, the 100 % reserve,
    the shadow balances and the Merkle leaves all count held cash (it is the
    client's and in custody), and `saldo_disponible` keeps meaning
    "spendable", so withdrawals, transfers, investments and AML holds are
    unchanged. The amount must buy at least one increment at the order's
    price.
  - A **sell** holds the units its amount sells at the order's price, from
    the free units (`mercado.posiciones.cantidad_comprometida`). From then
    on a market sale sells only free units (422 `POSICION_INSUFICIENTE`
    otherwise), and the vault's "Max" is the free units' value.
- **Rules at placement:** verified KYC, an active account (a preventive AML
  freeze answers the constant `OPERACION_NO_DISPONIBLE`), an asset that
  trades and is offered (real price, or simulated with `MODO_DEMO`), a stop
  the price has not reached yet (a buy stop above the price, a sell stop
  below; 422 `PRECIO_STOP_INVALIDO`), the cash or the free units, and at
  most 50 open orders per client (422 `LIMITE_ORDENES_PENDIENTES`).
- **Execution.** The trigger engine (`MERCADO_DISPARO_INTERVALO_SEG`,
  default 5 s, 0 disables it) fills, oldest first, every pending order whose
  asset trades and whose price the current one reaches: a limit at its price
  or better, a stop once its price is touched. A limit already reachable when
  placed (buy at 1,700 or less with the price at 1,612.40) fills at once, in
  the same request, through the same code.
  - A **buy** closes `filled`, releases its cash and buys with all of it at
    the current price, exactly as a market buy (AML evaluated before any
    write); the rest stays available.
  - A **sell** closes `filled`, releasing its units, and sells exactly those
    units at the current price.
  - The fill is a row of the market ledger with the key
    `ord:<order>:ejecucion`, and the database checks it against the pending
    order's terms: never worse than a limit, never before a stop, the same
    amount, exactly the held units.
  - A rule that no longer holds (KYC revoked, an account frozen by the
    operator or by AML, an amount that no longer buys one increment after a
    stop gapped up) closes it `rejected` with the error code in
    `reject_reason`, and what it held is released. A suspended asset or a
    transient error leaves it pending for the next pass. Concurrent engines
    (several replicas) and a cancel racing a fill are serialized by the
    account and order locks: an order closes once.
- **What the database guarantees** (`mercado.ordenes_pendientes` and deferred
  checks at COMMIT): an order is born pending; its terms never change; its
  state only moves forward; each account's held cash equals its pending buys
  and each position's held units its pending sells
  (`SC_COMPROMISO_DESCUADRE`); every hold and release is paired with its
  order in the same transaction (`SC_COMPROMISO_SIN_ORDEN`,
  `SC_ORDEN_SIN_COMPROMISO`); held cash is replayed from the ledger like the
  other balances (`SC_SALDO_SIN_LEDGER`); a closed account holds nothing.
  `sc_mercado` may only close an order (state, reason and links), never edit
  its terms; `sc_cliente` reads only its own orders (RLS).
- **The vault** (`GET /api/v1/vault` and the WebSocket): `balance.held_cash`
  (counted in `total_mxn`, not in `available_cash`) and each holding's
  `held_shares`. Placing, cancelling, filling or rejecting an order raises
  the vault's `seq`, a sell order included (it moves no cash: the account's
  order version does), so every change is pushed live. The ledger feed shows
  "Held for order" and "Order hold released"; the Account tab shows the cash
  held for open orders.
- **Audit:** `GET /api/v1/auditoria/ledger/ifpe` replays the held cash with
  the other balances; `/mercado` also reconciles held cash and units against
  the pending orders and every hold or release against its order.
- **Frontend: placing them.** The checkout drawer places limit and stop
  orders through the same endpoint. Before confirming, its summary says what
  stays held until the order fills (the amount for a buy, the units for a
  sell). The receipt follows the answer: "Limit buy placed" with what fills
  it and what it holds, "Order filled" when a limit was already reachable,
  or "Order not filled" with the reason. The asset page validates the
  answer (`parsePendingOrder`) and refreshes the open orders.
- **Frontend: Open orders** (`frontend/src/components/markets/open-orders-list.tsx`,
  on the Vault above the assets, absent when there are none):
  - One row per order: the ticker (it opens the asset's page), the kind
    ("Limit buy", "Stop-loss") and what fills it ("at $4,200.00 or lower",
    "when the price falls to $4,000.00"), its amount with what it trades (an
    estimate for a buy, at its own price and in the asset's increment;
    exactly the units a sell holds), and what it holds, with a lock. The
    header adds up the cash held, from the vault; the balance card links to
    the list from its "held for open orders" line.
  - Cancel is optimistic (`frontend/src/store/orders.ts`): the row leaves at
    once and `POST /api/v1/market/orders/{id}/cancel` goes out. A failure
    brings it back and says why; an order that filled or was rejected
    meanwhile (409) is gone anyway; a load that started before can never
    bring a cancelled order back. The released cash or units come back
    through the vault's WebSocket, never through arithmetic in the page.
  - The list reloads whenever the vault moves (the WebSocket's new version),
    so an order placed on another device appears and a filled one goes.
  - On a phone the amount and what is held move under the order, and the
    rows fit the width, Cancel included.
- Tests:
  - `tests/db/0015_ordenes_avanzadas.sql`: holds, pairing, immutability,
    validation of each fill against its order, the free-unit rule and the
    global invariants, in one transaction (the numbers of the Rust tests).
  - `backend/tests/ordenes_avanzadas.rs`: the API and the engine (holds and
    the reserve cut, idempotency, the list, cancelling, fills at each price,
    a limit filled at once, rules, rejections that release, the AML freeze,
    a cancel racing a fill, tamper detection).
  - `backend/tests/boveda.rs`: placing, cancelling and filling, pushed over
    the WebSocket.
  - `frontend/src/lib/orders.test.ts` and `frontend/src/store/orders.test.ts`:
    how an order is described, and the open orders store (validated loads,
    optimistic cancel, its rollback and the 409, a stale load).
  - `frontend/e2e/smoke.mjs`: a limit buy placed in the checkout (its held
    amount before and after), listed on the Vault above the assets; a
    stop-loss placed on another device appears there live; both cancelled
    from the list, the cash back through the WebSocket; the rows on a phone.
    Also a limit buy from another device shows its hold in the Account
    tab's ledger, and cancelling releases it.

## Pro chart and portfolio allocation (frontend)

Two analytics views in the Sovereign Gold identity, in native SVG and HTML (no
charting library, nothing new for the CSP). Money stays exact: decimal strings
and BigInt (`frontend/src/lib/decimal.ts`); only pixel positions and axis
labels use floating point.

- **Pro chart** (`frontend/src/components/charts/pro-candlestick-chart.tsx`,
  on the asset page):
  - A gold line or candlesticks. A rising candle (close at or above its open)
    is hollow green, a falling one solid red, so direction never rests on
    color alone. The toggle sits beside the ranges, and the device remembers
    the choice (`localStorage`, a convenience only).
  - Both modes share one window and one set of axes (`priceView`): whole
    candle periods from the one the range starts in to the one forming now.
    The period fits the range: 30 minutes over a day, 4 hours over a week, a
    day over a month, a week over a year.
  - The crosshair snaps to the nearest point or candle. Its exact price is
    tagged on the price axis and in the headline, and its date and time on
    the time axis. The legend reads the candle's open, high, low, close,
    change and volume, or the line's high, low and change over the range.
    The volume is its own pane under the price, on the same time axis. The
    keyboard (arrows, Home, End, Escape) reads each point or candle aloud.
- **Candles until the oracle serves them** (`frontend/src/lib/candles.ts`):
  - One candle per point of the quote's history: it opens at the point
    before and closes at its own price. Grouping into periods is exact.
  - For simulated prices (`price_source`, demo mode) the candles also get
    simulated wicks and volume. They are seeded with the ticker and the
    instant, so a quote always draws the same candles, and the last 24 hours
    of volume add up to the quote's `volume_24h` exactly. Live prices never
    get invented wicks or volume.
- **Allocation** (`frontend/src/components/vault/portfolio-analytics.tsx`, on
  the Vault under the balance):
  - The invested and cash shares, then a segmented bar: cash in gold, the
    investments largest first on a zinc scale. Beside it, the list of parts
    with value and share; holdings open their asset's page.
  - The math (`frontend/src/lib/allocation.ts`): cash is the total minus what
    is invested (available, held for open orders, on hold); each holding at
    its market value; fixed term is what is invested beyond the holdings
    (SOFOM contracts). Shares are basis points that add up to exactly 100.00 %
    (largest remainder). At most five parts: past four investments the
    smallest fold into Other, because four zinc steps are all that stay apart
    on the charcoal surface.
  - It follows `useVaultStore`, so the vault's WebSocket keeps it live.
- Tests:
  - `frontend/src/lib/candles.test.ts`: candles from the history, simulated
    wicks and volume, exact grouping, each range's window, the time axis, in
    any time zone.
  - `frontend/src/lib/allocation.test.ts`: the parts, Other, and shares that
    add up to 100.00 %.
  - `frontend/e2e/smoke.mjs`: the chart as a line and as candles (crosshair,
    legend, keyboard, ranges, the remembered type, the phone width). The
    Vault's allocation against the shares the vault's own figures give,
    following a deposit and a buy live.

## Layout

```
migrations/   SQL schema (sqlx-cli); 0001 approved and immutable
backend/      Rust: axum + sqlx + tokio
  src/db.rs              role-scoped transactions (the only place that calls begin())
  src/ledger/            IFPE engine + SHA-256 hash-chain verifier
  src/crypto/merkle.rs   Merkle sum tree (Proof of Reserve)
  src/reserva.rs         reserve cuts and per-client inclusion proofs
  src/sofom/             IFPE -> mutuum bridge, maturity liquidation, loans
  src/kyc/               onboarding: CURP/RFC/INE/MRZ validation, biometric provider, verdicts
  src/spei/              CLABE, signed SPEI webhooks, outbound dispatcher, gateway
  src/boveda/            Sovereign Vault: snapshot, LISTEN/NOTIFY fan-out, WebSocket
  src/routes/            HTTP API (/api/v1)
frontend/     Next.js app: the client dashboard (tabs Vault, Account, Invest, Reserve), onboarding,
              the compliance console; "Sovereign Gold" theme tokens in src/app/globals.css
tests/db/     SQL invariant tests for the schema
terraform/    AWS reference architecture (VPC, RDS, ECS Fargate, ALB + WAF)
nginx/        TLS edge for the single-host deployment
deploy/       compose settings template; PostgreSQL schema-owner init script
scripts/      host hardening, secrets generation and rotation, DB client, dev certificates
docker-compose.yml       single hardened host (see DEPLOYMENT.md)
docker-compose.dev.yml   local PostgreSQL for development
```

Production provisioning, releases, migrations, DNS/TLS, secrets rotation and the
launch checklist: [DEPLOYMENT.md](DEPLOYMENT.md).

## Running locally

```bash
cp .env.example .env                          # set JWT_SECRET and MFA_CLAVE_CIFRADO; MODO_DEMO=true for the demo
docker compose -f docker-compose.dev.yml up -d   # PostgreSQL 16 on 127.0.0.1:5432 (or a local one)
cd backend && cargo run                       # applies migrations and starts on :8080
```

Tests (need a user with `CREATEDB` and `CREATEROLE`). On a brand-new PostgreSQL cluster, run
the migrations once first (`cargo run`, above, does it): the tests migrate their databases
in parallel and would otherwise race to create the cluster-wide `sc_*` roles. CI does the same.

```bash
cd backend && DATABASE_URL=postgres://synthetic:synthetic@localhost:5432/synthetic_capital cargo test
# one test binary at a time, e.g. the AML/PLD suite on its own:
cd backend && DATABASE_URL=... cargo test --test aml
# SQL invariants: run on an empty, freshly migrated database (everything is rolled back).
psql "$EMPTY_DATABASE_URL" -f tests/db/0001_invariantes.sql
psql "$EMPTY_DATABASE_URL" -f tests/db/0006_kyc.sql
psql "$EMPTY_DATABASE_URL" -f tests/db/0007_kyc.sql
psql "$EMPTY_DATABASE_URL" -f tests/db/0008_spei.sql
psql "$EMPTY_DATABASE_URL" -f tests/db/0009_aml.sql
psql "$EMPTY_DATABASE_URL" -f tests/db/0010_auth_mfa.sql
psql "$EMPTY_DATABASE_URL" -f tests/db/0011_rls_por_cliente.sql
psql "$EMPTY_DATABASE_URL" -f tests/db/0012_boveda_tiempo_real.sql
psql "$EMPTY_DATABASE_URL" -f tests/db/0013_mercado_valores.sql
psql "$EMPTY_DATABASE_URL" -f tests/db/0015_ordenes_avanzadas.sql
cd frontend && npm test && npm run build
FRONTEND_URL=http://localhost:3000 node e2e/smoke.mjs   # needs backend (MODO_DEMO=true) + next start
# AML/PLD console against the real stack (structuring -> freeze -> four-eyes unfreeze, roles, no tipping off):
cd frontend && e2e/aml-stack.sh start && node e2e/aml.mjs; e2e/aml-stack.sh stop
```

The browser tests need Playwright and Chromium (`NODE_PATH` pointing at a
global `playwright` install, `PLAYWRIGHT_BROWSERS_PATH` at its browsers).
`e2e/aml-stack.sh start` creates a fresh database (`DB`, default `sc_e2e_aml`,
through the `PGHOST`/`PGUSER`/`PGPASSWORD` variables), starts the backend with
`MODO_DEMO` and two compliance officers, an operator and an auditor, and serves
the frontend with `next start`; `stop` kills both by PID and drops the database.

### Dependency audits

Both image builds fail when a locked dependency has a known vulnerability. The
checks run in an `audit` stage of each Dockerfile, alongside the compilation.
CI passes `--build-arg AUDIT_EPOCH=<run id>`, so a cached layer never hides an
advisory published after the lockfile last changed. Locally:

```bash
# Rust: Cargo.lock against the RustSec database (cargo install cargo-audit --locked).
# Policy in backend/.cargo/audit.toml: vulnerabilities, unsound and yanked crates fail.
cd backend && cargo audit
# Node: runtime dependencies fail from "moderate", the whole tree (build tools
# included) from "high".
cd frontend && npm audit --omit=dev --audit-level=moderate && npm audit --audit-level=high
```

- **Fix first.** Update the dependency (`cargo update -p <crate>`, `npm update
  <package>` or an `overrides` entry). Never relax the gate itself.
- **Rust exceptions** go in `ignore` in `backend/.cargo/audit.toml`, each with
  its ticket: why it does not apply, who approved it, when it expires. Today
  there is one, RUSTSEC-2023-0071 (`rsa`): it is in Cargo.lock only through
  sqlx's optional MySQL driver and is never compiled. The backend build fails if
  `rsa` ever enters the build graph.
- **npm** has no exception file. A finding with no fixed version is pinned
  through `overrides` or removed.

### Production database users

The backend uses two database identities:

| Identity | Variable | Used by |
|---|---|---|
| Schema owner (needs `CREATEROLE` the first time; on RDS, the master user) | `DATABASE_URL_PROPIETARIO` | only the one-shot `synthetic-capital migrar` |
| App login `sc_app`: `NOINHERIT`, member of `sc_ifpe`, `sc_sofom`, `sc_puente`, `sc_auditor`, `sc_kyc`, `sc_kyc_verificador`, `sc_spei`, `sc_aml`, `sc_auth`, `sc_cliente` and `sc_mercado` | `DATABASE_URL` | the long-running service |

The owner can disable triggers; the app login has no privileges of its own and
acts only through `SET LOCAL ROLE` to one `sc_*` role per transaction. By
default (no owner URL, `MIGRAR_AL_INICIO=true`, `ARRANQUE_AL_INICIO=true`) the
server migrates and bootstraps at startup with `DATABASE_URL`, and logs a
warning when that user is the owner: fine for development, not for production.
In production:

```bash
# 1. One-shot job, as the owner: migrations, bootstrap and the app login. Exit 0 or 1.
DATABASE_URL_PROPIETARIO_FILE=/run/secrets/database_url_owner \
DB_APP_PASSWORD_FILE=/run/secrets/db_app_password \
synthetic-capital migrar

# 2. The service, as sc_app only: the owner URL is not in its environment.
DATABASE_URL_FILE=/run/secrets/database_url JWT_SECRET_FILE=/run/secrets/jwt_secret \
MIGRAR_AL_INICIO=false ARRANQUE_AL_INICIO=false \
synthetic-capital
```

`migrar` always migrates and bootstraps (the `*_AL_INICIO` flags only concern
the server) and needs neither `JWT_SECRET` nor the webhook secrets. Give it the
same `MODO_DEMO`, `KYC_*`, `RESERVA_*`, `CAJA_*` and `BOOTSTRAP_*` values as the
service: the bootstrap writes them to the database. With `DB_APP_PASSWORD` set,
it then does the following in one transaction:

- creates or alters the login `DB_APP_USUARIO` (default `sc_app`) as
  `LOGIN NOINHERIT`, setting the password as a SCRAM-SHA-256 verifier
  (PBKDF2-HMAC-SHA-256, 4096 iterations, random 16-byte salt) computed by the
  backend, so the plaintext never reaches the server or its logs;
- grants the ten `sc_*` roles above (`WITH INHERIT FALSE, SET TRUE` on
  PostgreSQL 16+, where inheritance is set per grant), never `sc_auditor_pld`;
- checks the result, and rolls everything back if the login is the owner, a
  member of the owner, a superuser, has `CREATEROLE`, `CREATEDB`,
  `REPLICATION` or `BYPASSRLS`, or inherits the privileges of any `sc_*` role.

Re-running it is safe and rotates the password: run `migrar` with the new
password, then restart the service with the new `DATABASE_URL` (sessions that
are already open keep working until then). `DATABASE_URL` carries the same
password, URL-encoded; a hex password (`openssl rand -hex 32`) needs no
encoding. The password must be at least 32 printable ASCII characters without
spaces: PostgreSQL normalizes passwords with SASLprep and the driver does not,
and for such passwords both agree. Without `DB_APP_PASSWORD`, `migrar` leaves
the login alone. The manual equivalent is:

```sql
CREATE ROLE sc_app LOGIN NOINHERIT PASSWORD '...';  -- ideally a SCRAM verifier, not plaintext
-- PostgreSQL 16+; on older versions drop the WITH clause (NOINHERIT applies).
GRANT sc_ifpe, sc_sofom, sc_puente, sc_auditor, sc_kyc, sc_kyc_verificador, sc_spei TO sc_app
  WITH INHERIT FALSE, SET TRUE;
-- Compliance console (/api/v1/aml) and the AML batch worker. Money paths do
-- not need it: they reach the engine through SECURITY DEFINER wrappers.
GRANT sc_aml TO sc_app WITH INHERIT FALSE, SET TRUE;
-- Two-step verification and password reset (0010); a client's own reads under RLS (0011).
GRANT sc_auth, sc_cliente TO sc_app WITH INHERIT FALSE, SET TRUE;
-- Market orders: quotes, orders and their cash through the vault (0013).
GRANT sc_mercado TO sc_app WITH INHERIT FALSE, SET TRUE;
-- Optional: an external job that re-verifies the whole AML audit log
-- (SELECT * FROM aml.verificar_bitacora(1)) and can do nothing else.
CREATE ROLE audit_job LOGIN PASSWORD '...';
GRANT sc_auditor_pld TO audit_job;
```

`sc_auditor` (Proof of Reserve, also used by client routes such as
`/tablero`) has no access to the `aml` schema; console auditors read it
through `sc_aml`, where every function checks their read-only role.

**Deploying migration 0009 (AML/PLD):** drain every instance running a
pre-0009 binary before applying it. 0009 revokes INSERT on `kyc.alertas_pld`,
so an old binary would fail every money transaction that raises an alert.

**Containers and secrets files.** The image (`backend/Dockerfile`, distroless,
uid 65532) runs the server by default and `migrar` as a one-shot command. Its
`HEALTHCHECK` runs `synthetic-capital salud vivo`, a probe of `GET /vivo` on
the loopback at the `BIND_ADDR` port that never touches the database, so a
PostgreSQL outage does not restart the container. Load balancers use
`GET /salud`, which checks the database (`salud listo` probes it). Every
configuration variable `NAME` (all but `RUST_LOG`) also accepts `NAME_FILE`:
the value is read from that file (trailing newline trimmed; setting both is an
error), so Docker and Kubernetes secrets stay out of the environment. The
process writes nothing to disk and runs with a read-only root filesystem.

### HTTP limits

The backend serves HTTP/1.1 itself with these limits; put a TLS reverse proxy
in front for production.

| Variable | Default | Effect |
|---|---|---|
| `HTTP_CABECERAS_SEG` | 10 | Close connections that do not send a full request head in time, including idle keep-alive and silent sockets |
| `HTTP_CUERPO_SEG` | 15 | Request body deadline (408 `TIEMPO_AGOTADO`) |
| `HTTP_PETICION_SEG` | 30 | Whole-request deadline (503 `TIEMPO_AGOTADO`; the transaction rolls back) |
| `HTTP_MAX_CONEXIONES` | 512 | Concurrent connections; beyond this the server stops accepting instead of running out of file descriptors. Also the cap on open vault WebSockets per task, counted separately |
| `HTTP_APAGADO_SEG` | 20 | Graceful shutdown deadline |

Request bodies are limited to 64 KB. Per-IP rate limits (`HTTP_LIMITE_*`) are
described under "Request hardening, rate limits and row-level security".

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
| POST | `/api/v1/auth/registro`, `/api/v1/auth/login` | public (sign-up starts with KYC `PENDIENTE`; staff sign-in needs the second step) |
| POST | `/api/v1/auth/verify-mfa`, `/api/v1/auth/mfa/setup`, `/api/v1/auth/mfa/enable` | two-step verification (see above) |
| POST | `/api/v1/auth/password/forgot`, `/api/v1/auth/password/reset` | public: password reset by single-use link |
| GET | `/api/v1/kyc/expediente` | client: onboarding state, attempts, last biometric result |
| POST | `/api/v1/kyc/perfil` | client: step 1, personal data, CURP, RFC and address |
| POST | `/api/v1/kyc/verificar` | client: steps 2-3, INE images and data + liveness frames + consent (up to 12 MB) |
| POST | `/api/v1/kyc/webhook` | biometric provider (HMAC signature `X-Kyc-Firma`) |
| GET | `/api/v1/operaciones/kyc/pendientes` | operator: files awaiting human review |
| POST | `/api/v1/operaciones/kyc/{cliente_id}` | operator: verify / reject (with reason) / reopen, risk level (not lowered while the client is under AML review); answers the client projection, without risk fields; an empty dictamen is 400 |
| GET | `/api/v1/tablero` | client: vault + investment + live reserve |
| GET | `/api/v1/vault` | client: Sovereign Vault snapshot (balance, reserve proof, holdings) |
| GET | `/api/v1/vault/ws` | client: Sovereign Vault real-time channel (WebSocket, subprotocol `sc.vault.v1`) |
| GET | `/api/v1/market/assets` | any session: the catalog that can be quoted and traded now (`MarketCatalog`) |
| GET | `/api/v1/market/quote/{ticker}` | any session: the asset and its price history (`MarketQuote`) |
| POST | `/api/v1/market/order` | client: order by amount (`OrderRequest`): market (`OrderReceipt`, filled at the current price), limit or stop (`PendingOrder`, its cash or units held until it fills) |
| GET | `/api/v1/market/orders/pending` | client: its limit and stop orders still waiting (`PendingOrders`) |
| POST | `/api/v1/market/orders/{id}/cancel` | client: cancels a pending order and releases what it held |
| GET | `/api/v1/ifpe/cuenta`, `/api/v1/ifpe/movimientos` | client |
| POST | `/api/v1/ifpe/transferencias` | client: internal transfer |
| POST | `/api/v1/ifpe/retiros` | client: withdrawal to own external CLABE (an outgoing SPEI) |
| POST | `/api/v1/ifpe/depositos/simulados` | client with verified KYC, `MODO_DEMO` only |
| GET | `/api/v1/sofom/tasas` | public: terms and rates |
| GET | `/api/v1/sofom/posicion` | client |
| POST | `/api/v1/sofom/mutuos` | client: **IFPE -> SOFOM bridge** |
| POST | `/api/v1/sofom/mutuos/{id}/liquidar` | owning client or operator (on maturity) |
| GET | `/api/v1/reserva/estado`, `/api/v1/reserva/cortes/ultimo` | public |
| GET | `/api/v1/reserva/cortes?antes=&limite=`, `/api/v1/reserva/cortes/{id}` | public: history of published anchors (root, totals, ledger sequence and hash) |
| GET | `/api/v1/reserva/prueba` | client: Merkle proof for their account |
| POST | `/api/v1/reserva/verificar` | public: verify a proof against the published root |
| GET | `/api/v1/auditoria/ledger/{ifpe,sofom,mercado}` | operator / auditor |
| POST | `/api/v1/operaciones/depositos` | operator: manual entry of an incoming SPEI (5-digit `banco_emisor`, `fecha_operacion`, `clave_rastreo`); same path and same identity as the webhook, so a payment entered by hand and notified by the provider is credited once |
| POST | `/api/v1/operaciones/creditos`, `.../{id}/pagos`, `.../{id}/castigo` | operator: loans, collections, write-offs (losses absorbed by SOFOM capital) |
| POST | `/api/v1/operaciones/usuarios/{id}/revocar-sesiones` | operator: invalidate a user's JWTs immediately |
| POST | `/api/v1/spei/webhook/inbound`, `/api/v1/spei/webhook/estado` | SPEI provider (HMAC signature) |
| POST | `/api/v1/spei/transfer` | client: send an SPEI |
| GET | `/api/v1/spei/transferencias`, `/api/v1/spei/recibidos` | client |
| GET | `/api/v1/spei/conciliacion`, `/api/v1/spei/en-revision` | operator / auditor |
| POST | `/api/v1/operaciones/spei/recibidos/{id}/correccion` | operator: reverse a wrong SPEI credit |
| POST | `/api/v1/operaciones/spei/recibidos/{id}/anulacion` | operator: void a wrong manual SPEI entry and free its identity |
| POST | `/api/v1/operaciones/spei/enviados/{id}/reembolso` | operator: refund an order in manual review that the provider never received |
| POST | `/api/v1/operaciones/spei/en-revision/{id}/descarte` | operator: discard a held SPEI notification as a duplicate (no money moves) |
| POST | `/api/v1/operaciones/...` | operator: holds (shown to the client as neutral `ret:`/`lib:` entries), KYC, capital |
| GET/POST | `/api/v1/aml/...` | compliance officer (auditor read-only): AML/PLD console, see above |

Full table in `backend/src/routes/mod.rs`.

**Roles.** `CLIENTE` (own account), `OPERADOR` (back office: SPEI entries,
holds, KYC review, loans; **no AML access**), `AUDITOR` (Proof of Reserve and
ledger audit; read-only on `/api/v1/aml`) and `CUMPLIMIENTO` (compliance
officer: the AML/PLD console under `/api/v1/aml`). Unfreezing a client,
releasing AML holds and changing risk rules need a second officer (four-eyes,
enforced in the database), so staff at least two `CUMPLIMIENTO` users.
Internal users are provisioned by the schema owner, like operators (on AWS
through the Session Manager tunnel, DEPLOYMENT.md §4.1); with
`MODO_DEMO=true`, `BOOTSTRAP_CUMPLIMIENTO_EMAILS` / `_PASSWORD` create demo
officers on startup, each named after the local part of their email
("Cumplimiento oficial1").
