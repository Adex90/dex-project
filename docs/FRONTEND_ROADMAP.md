# Frontend Roadmap

The 32 modules of the Synthetic Capital client app, in six architectural phases.
Tick a module's box when it meets the definition of done below. Most modules
take several pull requests, each green in CI.

The short notes under a module record what already exists in this codebase or
what the module waits on. They do not change its scope.

## Before Phase 1: open decisions

- **Brand color.** "Sovereign Gold" shipped with Synthetic Capital Yellow
  `#EAB308`, the `--color-synthetic-yellow` token in
  `frontend/src/app/globals.css`. This roadmap's brief names `#F59E0B`.
  - Switching is one token, plus two color assertions in
    `frontend/e2e/smoke.mjs`.
  - `#F59E0B` sits close to the orange warning token (`#FB923C`); warnings
    would need a new hue so they are not mistaken for brand.
  - Confirm the value before Phase 1 adds more yellow.
- **Contracts before screens.** The backend has no instruments, quotes, orders,
  FX, options or order book yet. Each Phase 1 module starts with:
  - a typed, reviewed API contract in `frontend/src/types/<domain>.ts`, with
    runtime validation in `frontend/src/lib/<domain>.ts`;
  - fixtures against that contract.

  This is how the vault was built: `types/vault.ts` first, then the Rust
  endpoints.
- **Licensing scope** (confirm with legal and compliance before production).
  - Today's backend is an IFPE (e-money, SPEI) plus a SOFOM (fixed-term
    mutuums).
  - Trading, options and margin (modules 1 to 7, 19 and 29) need a securities
    intermediary: a casa de bolsa license or a partner broker.
  - The Ley Fintech bars an IFPE from paying interest on e-money balances. In
    the current design yield comes only from SOFOM contracts. Module 13's
    counter is a labelled preview on IFPE cash: before anything is paid, the
    yield needs a structure the licence allows (a SOFOM contract the client
    opts into, a fund, or a banking licence).
  - Margin (modules 19 and 29) is credit. The app's 100% reserve promise says
    vault money is "never lent", so this is also a product decision.
- **Navigation.**
  - The phone tab bar holds five destinations at most; today it shows four
    (Vault, Account, Invest, Reserve).
  - Decide the top-level tabs before Phase 2 adds Markets. One proposal:
    Vault, Markets, Portfolio, Cash, Profile, with the rest one level down.

## Definition of done (every module)

- **Sovereign Gold:**
  - only the theme tokens, never raw colors;
  - the yellow for primary actions, active states and brand accents;
  - green and red only for gains, losses and states.
- **Layout:** works at 390 px with the bottom tab bar and from `lg` with the
  sidebar terminal layout.
- **Accessibility:** WCAG 2.2 AA contrast, full keyboard use, and named
  controls for screen readers.
- **Money:** decimal strings and `lib/decimal.ts` (BigInt), never floating
  point. Every operation that moves money carries an idempotency key, as the
  API requires.
- **Network data:** validated before it reaches a store; a malformed payload is
  rejected whole.
- **Real time:** streams follow the vault channel's protocol
  (`lib/vault-socket.ts`): snapshot first, ordered by `seq`, resync on gaps,
  backoff on drops. Prefer one multiplexed market-data socket to one per
  instrument; the edge allows 20 sockets per address.
- **Testing:** pure logic lives in `src/lib` with `node --test` unit tests.
  `npm run typecheck`, `npm run lint`, `npm test` and `next build` pass. New
  user flows extend `e2e/smoke.mjs`, and `e2e/aml.mjs` stays green.
- **Dependencies and CSP:**
  - A new dependency passes the `npm audit` gate (Dockerfile and CI).
  - Third-party scripts or connections need a deliberate change to the edge
    CSP (`nginx/snippets/security-headers.conf`), which today allows only the
    app's own origin.
- **Unfinished backends:** a module whose API is not live yet stays off the
  navigation.

## Phase 1: Core Trading & Execution

- [x] **1. Asset Detail View** (Chart, Key Stats, Sticky Buy/Sell)
  - `components/markets/asset-detail.tsx`, with the pro chart (module 10);
    the vault's balance chart stays on `components/charts/trend-chart.tsx`.
  - Mounted at `/dashboard/asset/[ticker]`, inside the dashboard shell (the
    Vault tab stays current; a "Back to vault" link returns). The quote comes
    from `GET /api/v1/market/quote/{ticker}` (`MarketQuote`, migration `0013`)
    with the session's token, validated whole (`lib/market-data.ts`); the cash
    and the position come from the vault store. An unknown ticker shows "No
    such asset"; without live prices (503) the page says the market is not
    open yet. Discover's rows and the holdings in the vault's asset list open
    it, and it stays under the tab that opened it (Back returns there).
  - Prices are simulated until the pricing oracle, and served only with
    `MODO_DEMO`. Smoke e2e covers the page on both layouts.
  - Left: a market-data stream on the vault socket's pattern (the quote is
    read once per visit).
- [x] **2. Order Execution Checkout** (Numpad, swipe-to-trade, zero-commission summary)
  - `components/markets/order-checkout-drawer.tsx`: numpad, $0.00 commission
    summary, Confirm Order, one idempotency key per order kept on retry.
    Order math in `lib/market.ts`, unit tested; the estimate for a sale
    follows the backend's rule ("Max" sells the whole position).
  - Wired to `POST /api/v1/market/order`. The receipt is validated before it
    is shown, and the page never writes the vault store: the order's commit
    raises the vault's `seq` and the WebSocket brings the new cash and
    position. Refusals (400, 409, 422, 503) show their reason in English.
  - Smoke e2e: a buy whose answer is lost is retried with the same key and
    fills once; "Max" sells the whole position on a phone.
  - Left: swipe-to-trade. Confirm Order is the keyboard-accessible path a
    swipe would need anyway.
- [x] **3. Advanced Order Engine** (Limit, Stop-Loss, Trailing, Day/GTC)
  - Live: the checkout places limit and stop orders (`LIVE_ORDER_TYPES`),
    their price typed on the same numpad as the amount, with their estimates
    and rules (`assessOrder` in `lib/market.ts`) and what stays held until
    they fill. The receipt says what the order waits for and what it holds,
    or that a reachable limit filled at once.
  - Backend (migration 0015, README "Limit and stop orders"): the held cash
    or units keep the 100 % reserve intact; the trigger engine fills each
    order at its price or rejects it and releases what it held.
  - Open orders on the Vault, above the assets
    (`components/markets/open-orders-list.tsx`): what each order waits for,
    its amount and what it holds, with an optimistic Cancel
    (`store/orders.ts`); the list follows the vault's WebSocket. Unit tests
    and smoke e2e (placed in the checkout, a stop-loss from another device
    appearing live, both cancelled, phone layout).
  - Left: trailing stops, time in force (Day/GTC) and a history of closed
    orders (filled, cancelled, rejected).
- [ ] **4. Savings Plans / DCA** (Automated fractional recurring buys)
  - Frontend built: Savings plan in the same toggle (buys only), with the
    amount per purchase, the frequency (weekly, every 2 weeks, monthly), the
    quantity at today's price and the monthly equivalent. Contract:
    `SavingsPlanRequest` (`frequency`).
  - Not live: the backend answers 422 `TIPO_ORDEN_NO_DISPONIBLE`. Left: the backend schedule (execution day,
    business days, what happens without cash), the plan's own response
    (next purchase date), and a screen to pause, edit or cancel plans.
- [ ] **5. Options & Derivatives Chain** (Calls/Puts, Greeks, Expirations)
  - Gated by module 19. Waits on options market data.
- [ ] **6. Multi-Currency FX Engine** (Live spread and exchange rates)
  - The ledger is MXN only today.
- [ ] **7. Orderbook L2** (Real-time market depth for Web Terminal)
  - Desktop terminal layout first (`lg` and up). Waits on a depth stream.

## Phase 2: Discovery & Pro Analytics

- [x] **8. Market Explorer** (Predictive search, trending, top movers, collections)
  - The Discover tab (`app/dashboard/discover/page.tsx`), in the sidebar and
    the phone's tab bar. The catalog comes from `GET /api/v1/market/assets`
    (migration `0014` adds a synthetic Tesla share and CETES at 28 days),
    validated whole and kept with the search in `store/market.ts`.
  - Search by ticker, name or class, accents and case aside, ranked ticker
    first (`lib/discover.ts`, unit tested). Without a query the list is
    Trending: the most traded over 24 hours first. Rows open the asset's
    page; the Vault's empty asset list points here. Smoke e2e covers it on
    both layouts, with a first buy from Discover.
  - Above the list, the day's top movers and the collections
    (`components/markets/discover-highlights.tsx`): one row that swipes on a
    phone, a grid from `sm`. Top movers: the largest 24-hour rise (green) and
    fall (red) in the whole catalog, compared exactly, each opening its
    asset. Collections (Hard Assets, Synthetic Equities, Fixed Income) list
    their assets and their equal-weight 24-hour change; picking one presets
    the screener's asset class (module 9). Both step aside while a search is
    typed, and everything comes from the catalog (`lib/screener.ts`, unit
    tested): no new endpoint.
  - Left: predictive search once the catalog outgrows a single page (a `q`
    parameter on the endpoint), and curated collections beyond asset classes
    (themes chosen by hand, which need a catalog field or an endpoint).
- [x] **9. Asset Screener** (Cross-filtering technicals/fundamentals)
  - `components/markets/asset-screener.tsx`, an inline panel above the list:
    toggles for the asset class (Equities, Commodities, Fixed income) and the
    24-hour performance (Positive, Negative), solid Synthetic Gold when on.
    Choices add up within a row and narrow across rows, and combine with the
    search; the list updates as each one changes. Each toggle shows what it
    would leave given the search and the other row (cross-filtering), and one
    that would leave nothing is disabled. A direction is the one the list
    prints (0.00 % is neither), so a filter never disagrees with a color.
  - Client-side over `GET /api/v1/market/assets` (`lib/screener.ts`, unit
    tested); the filters live in `store/market.ts` with the search, so they
    survive a visit to an asset. The list, its summary and the screen reader
    status say what is shown; a search the filters exclude offers to clear
    either. Smoke e2e: collections, toggles, counts, the disabled toggle, the
    empty state and the return from an asset.
  - Left: sorting (by change, price, yield), ranges on the figures (market
    cap, volume, yield), and technical screens once there is a candle feed.
- [x] **10. Pro Charting** (TradingView wrapper, candlesticks, indicators, drawing tools)
  - `components/charts/pro-candlestick-chart.tsx` on the asset page, in native
    SVG: no charting library, so nothing to license and no change to the CSP.
    A gold line or candlesticks (rising: hollow green; falling: solid red),
    switched by a toggle beside the ranges and remembered on the device. Both
    modes share the window and the axes. The candle period fits the range: 30
    minutes over a day, 4 hours over a week, a day over a month, a week over a
    year.
  - The crosshair snaps to the nearest point or candle. Its exact price is
    tagged on the price axis and shown in the headline; its date and time on
    the time axis. The legend reads the candle's open, high, low, close,
    change and volume. The volume is a pane of its own under the price (no
    second scale), and the keyboard reads every point or candle aloud.
  - Candles come from the quote's history (`lib/candles.ts`, unit tested):
    each point opens at the one before, and grouping is exact (first open,
    highest high, lowest low, last close, summed volume). Simulated prices get
    simulated wicks and volume, seeded so a quote always draws the same, with
    the last 24 hours adding up to the quote's 24-hour volume. Live prices
    never get invented ones. Smoke e2e: line, candles, crosshair, keyboard,
    ranges, the remembered type and the phone width.
  - Left: candles and volume served by the price oracle, indicators (moving
    averages, RSI), drawing tools and a live price stream.
- [x] **11. Portfolio Analytics** (Donut charts, sector/geo distribution, dividend tracker)
  - `components/vault/portfolio-analytics.tsx` on the Vault, under the balance.
    It shows the invested and cash shares and a segmented bar: cash in gold,
    then each investment, largest first, on a four-step zinc scale checked
    against the charcoal surface. Beside the bar, the list of parts with value
    and share; a holding's row opens its asset. Pointing at a block or a row
    lights up both.
  - Exact (`lib/allocation.ts`, unit tested). Cash is the total minus what is
    invested: available, held for orders and on hold. Fixed term is what is
    invested beyond the holdings. Shares are basis points that add up to
    100.00 % (largest remainder), and past four investments the smallest fold
    into Other. Live from `useVaultStore`. Smoke e2e checks the shares against
    the vault's own figures, and that they follow a deposit and a buy live.
  - Left: sector and geography (the catalog has no such fields yet),
    dividends and yield income, and performance over time (time-weighted
    return).
- [ ] **12. Corporate Events Calendar** (Earnings, ex-dividend dates)

## Phase 3: Wealth & Liquidity

- [x] **13. Cash/Yield Tab** (Uninvested balance and live interest counter)
  - On the Account tab (`components/wealth/cash-yield-section.tsx`), above
    the account: the uninvested cash (`available_cash` from `useVaultStore`)
    and a live "Earned this month" counter at a mocked 7.25 % Sovereign Yield
    (ACT/360, exact to 10^-8 and rounded down, `lib/wealth.ts`, unit
    tested). It ticks on every animation frame without re-rendering React
    (once a second with reduced motion), shows enough decimals for the last
    one to move, and adds the month so far, a day's yield and the month's
    projection.
  - A preview, labelled as such: simulated on today's available cash since
    the 1st, and nothing is paid (see the licensing scope above). Smoke e2e
    checks the counter against the exact accrual at the instant it is read,
    and that it ticks up.
  - Left: the payout, through a structure the licence allows, and then the
    counter on real accrual (time-weighted from the ledger, not today's
    balance), with the SOFOM contracts' accrual beside it.
- [x] **14. Card Management** (Virtual card UI, freeze, PIN, Apple/Google Wallet)
  - On the Account tab (`components/wealth/card-management.tsx`): the card
    face (pure black and square, the gold mark, a minimal chip, the number
    masked until issued, the holder's name as a card carries it). Lock is a
    toggle that frosts the card and stays gold while on ("lock", never
    "freeze": client screens must not use AML hold words); Show PIN opens
    the PIN's place for 10 seconds (a preview card has none yet); Settings
    turns the card's uses on and off (online, contactless, ATM, abroad). A
    Saveback tracker shows 1 % of card payments toward a $300.00 monthly
    cap, on simulated spending. State in `store/card.ts`, reset on sign-out.
  - A preview, labelled as such: no card is issued and nothing is sent.
    Smoke e2e covers the lock (kept across tabs), the PIN's place closing by
    itself, the settings and the phone layout.
  - Left: a card issuing processor (the number, the PIN, locking and the
    uses through its API). Adding a card to Apple or Google Wallet from
    inside the app needs native apps; a web app cannot do it.
- [ ] **15. Round-up & Saveback** (Cashback tracking and spare change investing)
  - Previewed on the card (module 14): a saveback tracker on simulated
    spending. Left: card payments to compute it from, round-ups, and where
    the money goes (cash, or a savings plan once module 4 is live).
- [x] **16. Linked Accounts & Withdrawals** (Whitelisting CLABE and fiat off-ramp)
  - The whitelist (migration 0016, README "Linked accounts and
    withdrawals"): `ifpe.cuentas_vinculadas` holds the external CLABEs a
    client may withdraw to. The holder must be the client's KYC name, the
    client must be able to operate, the CLABE is never one of ours and is
    active for one client only; rows are never deleted, only retired. Every
    own-account withdrawal goes only to a linked account, on both routes,
    and the order is tied to it (checked again by the database).
  - The off-ramp: `POST /api/v1/wealth/withdraw` sends available cash, never
    cash held by open orders, by SPEI. It leaves the available cash at once
    with a RETIRO and stays "processing" (the SPEI order, PENDIENTE then
    ENVIADA) until the network settles it, or comes back if the bank
    returns it. The balance change bumps the vault's `seq`, and the
    WebSocket pushes it. `GET /api/v1/wealth/linked-accounts` lists the
    accounts (bank, last four digits, holder; never the full CLABE).
  - The order of checks keeps AML holds undisclosed: the whitelist is
    checked after the AML evaluation, so a frozen client gets the neutral
    refusal, never "not linked".
  - The UI: a Withdraw button beside the Sovereign Yield counter opens
    `components/wealth/withdrawal-drawer.tsx`, a checkout in three steps.
    The account step shows the linked accounts with the holder and "In your
    name". The amount step is a numpad against the available cash, with Max
    rounded down to the cent and a note when open orders hold cash. The
    confirm step has a stark gold button. The receipt says "processing" and
    gives the SPEI tracking key. One idempotency key per account and amount.
  - Demo mode links one account per verified client on first look: BBVA
    México, in the client's KYC name, with a valid CLABE ending in 1234.
  - Tests: `tests/db/0016_cuentas_vinculadas.sql`, `backend/tests/patrimonio.rs`,
    the WebSocket push in `backend/tests/boveda.rs`, `lib/withdrawal.test.ts`,
    and smoke e2e (the whole flow on a desktop and a phone).
  - Payments to someone else's account (`POST /api/v1/spei/transfer`) are
    off by default (`SPEI_TRANSFERENCIAS_TERCEROS=false`, 422
    `TRANSFERENCIAS_TERCEROS_NO_DISPONIBLES`): money leaves only for an
    account in the client's own name, which limits what a taken-over
    session can drain. Decided with the product owner after this module.
  - Left: linking a real account. The decided design is an STP penny drop
    to the CLABE plus the holder's name on Banxico's CEP for it, matched
    against the KYC name. Until that exists no account is linked outside
    demo mode, so production cannot withdraw yet. Also left: unlinking from
    the app, a limit on accounts per client, and the withdrawal's later
    states (settled, returned) in the app beyond the ledger.
- [ ] **17. SPEI Dispute Tracker** (Banxico CEP integration for delayed transfers)
  - Orders already carry their tracking key (`clave_rastreo`). The lookup of
    Banxico's CEP (electronic payment receipt) goes through the backend.

## Phase 4: Identity, Security & KYC

- [x] **18. AML/KYC Onboarding** (ID scan, liveness check, contract signing)
  - The wizard: `/dashboard/verify`, inside the dashboard, where "Complete
    verification" leads (`/auth/onboarding` redirects there). Four steps
    under a gold progress bar: (1) legal name, CURP and RFC checked as
    typed, with the date of birth and nationality read from the CURP, and
    the address; (2) the INE's two sides by camera, upload or drag and drop,
    previewed and validated in the browser, and its printed details;
    (3) biometric consent and the live camera check (frames stay in memory),
    then occupation, source of funds and the politically exposed person
    (PEP) question, none with a default; (4) a review of every answer with
    Edit, the three agreements (account and custody agreement, privacy
    notice, sworn statement) accepted one by one, and a gold "Submit for
    Verification". Then the status page.
  - The contract (migration 0017, README "Digital onboarding and biometric
    KYC"): the declarations and the accepted agreements travel with the
    existing `POST /api/v1/kyc/verificar`; no endpoint was added. The
    database requires them on submission, records the agreements' version
    in force (`kyc.parametros.contratos_version`) and the time, freezes them
    with the identity, and copies occupation and source of funds to the
    client on verification. A declared PEP never passes on the automatic
    check: the file goes to a person, and only an operator can verify it;
    the client is then marked PEP with high risk.
  - In demo mode the simulated provider answers at once. When the file is
    VERIFICADO the page reloads the dashboard's summary, so deposits,
    withdrawals and trading unlock without a page reload.
  - The new screens carry none of the words that would tip a client off
    about an AML hold; the AML e2e checks the declarations and the
    agreements, collapsed text included.
  - Tests: `tests/db/0017_kyc_declaraciones.sql`, the declarations and
    agreements and the PEP path in `backend/tests/kyc.rs`, the routing,
    declarations and agreements in `components/onboarding/logic.test.ts`,
    smoke e2e (the whole wizard, and the unlock with no reload), and the
    AML e2e.
  - Left: passports (the backend verifies INEs against the electoral
    registry only), a real identity and biometric provider instead of the
    simulated one, a console screen for an operator to approve a PEP's file
    (today `POST /api/v1/operaciones/kyc/{cliente_id}`), the agreements'
    full legal text with a downloadable signed copy, and a way to collect
    the declarations of clients verified before migration 0017, who have
    none on file.
- [ ] **19. Investor Profiling & Quizzes** (Gatekeeping Margin/Options tiers)
  - Gates modules 5 and 29.
- [x] **20. Security Center** (FaceID/TouchID, PIN reset)
  - The Security page, `/dashboard/security` (`components/security/`): in
    the sidebar's Account group, gold when current, and on phones behind the
    header's lock button (the tab bar keeps five tabs). Besides the devices
    of module 21: a password change (current password, new one with a live
    checklist of the server's rules, and the authenticator code when
    two-step verification is on); the inactivity timeout (5, 15, 30 or 60
    minutes, saved on pick); two-step verification's status, with a way to
    turn it on that reuses `/auth/mfa-setup` outside the dashboard; passkey
    readiness for Face ID, Touch ID or Windows Hello, read from the browser
    (status only); and the security log in words.
  - The contract (migration 0019, README "Security Center: per-device
    sessions"): `POST /api/v1/auth/password/change` counts every try toward
    the sign-in lockout before verifying, requires the new password to
    differ, signs out every other device, voids a pending reset link, and
    ends the session that exhausts the tries. `GET`/`POST
    /api/v1/auth/security/preferences` and `GET /api/v1/auth/security/events`.
    Every change goes to the append-only `kyc.eventos_autenticacion`.
  - Tests: `tests/db/0019_sesiones_seguridad.sql`, `backend/tests/sesiones.rs`,
    `lib/security.test.ts`, smoke e2e (the password change and its refused
    try, the timeout surviving a reload, two-step verification turned on
    from Security on a phone, the inactivity sign-out on a fake clock) and
    the AML e2e (a frozen client's Security tab and its API reads carry no
    tipping-off words).
  - Left: registering and signing in with passkeys (WebAuthn: the backend
    has no credential storage yet), moving two-step verification to a new
    phone without support, and recovery codes. The card PIN lives with the
    card preview (module 14).
- [x] **21. Advanced Session Management** (Active devices, remote logout)
  - Every sign-in is now a row in `kyc.sesiones` and its token carries the
    row's id; each request checks the row is not revoked, not expired and
    used within the client's inactivity timeout. Tokens issued before 0019
    are refused, so deploying it signs everyone out once.
  - The Active devices panel: every session, this one first with the gold
    "Current device" badge, its device (summarized from the User-Agent by
    the server, never stored raw), sign-in IP, sign-in time, last activity
    and end. Revoke signs one device out; "Sign out all other devices" asks
    once, with the count. A revoked device's next request fails, and its
    live vault connection closes within a minute. Signing out ends the
    session on the server too, and an unattended tab signs itself out after
    the timeout, with a notice saying why.
  - Endpoints: `GET /api/v1/auth/sessions`,
    `POST /api/v1/auth/sessions/{id}/revoke`,
    `POST /api/v1/auth/sessions/revoke-others`, `POST /api/v1/auth/logout`.
    A password reset, turning two-step verification on and an operator's
    revocation also close the rows.
  - Tests: as module 20, plus the vault socket closing for one revoked
    session while another stays (`backend/tests/boveda.rs`), and in the
    smoke e2e Revoke and the panic button checked against the tokens.
  - Left: approximate location from the IP (needs a GeoIP source and a
    privacy review), notifying the client by email of a new device, and
    showing the user agent's version.
- [ ] **22. Step-Up Authentication** (Forced biometrics on anomalous withdrawals)
  - Builds on modules 20 and 16. The anomaly signal comes from the backend's
    AML engine.

## Phase 5: Compliance & Transparency

- [ ] **23. Activity Feed** (Immutable ledger of deposits, trades, and dividends)
  - Foundation: the Account tab's SHA-256-chained ledger feed. Trades and
    dividends arrive with Phase 1.
- [x] **24. Document Inbox** (PDF statements, trade confirmations)
  - The Documents page, `/dashboard/documents` (`components/documents/`),
    in the sidebar's Account group and, on phones, in the strip of account
    pages on the Account tab and on each account page. An inbox of every
    trade confirmation (Comprobante de Operación) and monthly account
    statement (Estado de Cuenta), newest first, filtered by kind, with date,
    folio, reference hash and amount; the month in progress says so.
  - The viewer: a confirmation's execution figures (side, order type and
    its limit or stop, amount requested, quantity, price and its source),
    the settlement (gross, a $0.00 commission marked Zero commission, net,
    and for a sale its cost basis and realized gain or loss) and both
    ledgers' entries and hashes; a statement's opening and closing balances,
    credits, debits, the closing breakdown (available, funds on hold with
    the neutral copy, held for open orders), its movements, trades,
    positions and content hash. Both carry the reserve seal: the first
    Proof of Reserve snapshot that covers them (100.00% backed, Merkle root,
    anchors), or pending until one does. Print / Save as PDF prints a paper
    copy in normal flow (every hash in full); Download (.json) saves the
    document as served. `?id=` opens one directly.
  - The contract (README "Document Inbox"): `GET /api/v1/wealth/documentos`
    and `GET /api/v1/wealth/documentos/{id}`, derived on every read from
    `mercado.ordenes` and the IFPE ledger, so an issued document never
    changes; months follow Mexico City.
  - Tests: `backend/tests/documentos.rs`, `lib/documents.test.ts`, smoke e2e
    (one confirmation per settlement in the cash ledger, the sale's figures,
    the seal before and after a snapshot, the statement against the
    account's balance, the download compared with the API, the printed page
    and both PDFs) and the AML e2e (a frozen client's statement and pages
    carry no tipping-off words).
  - Left: server-side PDF generation with a digital signature (today the
    browser prints), closed months materialized once instead of derived on
    every read, and the SAT's CFDI for interest (module 26).
- [x] **25. Legal Beneficiaries** (Assigning heirs and succession percentages)
  - The Beneficiaries page, `/dashboard/beneficiaries`
    (`components/beneficiaries/`): up to five people with their full legal
    name, relationship, CURP or date of birth (a valid CURP fills in the
    date) and share. A gold allocation bar counts the shares in cents of a
    percent and reads "100% allocated" or what is left or over; Save stays
    off until the sum is exactly 100%. Split evenly gives 33.34, 33.33 and
    33.33. The record shows the version on record, its history and the
    server's chain verification.
  - The contract (migration 0020, README "Legal beneficiaries"):
    `GET`/`POST /api/v1/kyc/beneficiarios`. Each save is a new immutable,
    hash-chained version on top of the one the page loaded (409 if another
    session saved first: the newer one shows and the edits stay). The
    database refuses any sum but exactly 100% at commit, the holder or a
    repeated CURP, and an unverified client; and writes every version to
    the client's security log, where the Security page shows "Beneficiaries
    updated".
  - Tests: `tests/db/0020_beneficiarios.sql`, `backend/tests/beneficiarios.rs`,
    `lib/beneficiaries.test.ts`, smoke e2e (60 + 30 refused, 60 + 40 saved,
    reloaded, split evenly, a save refused after another device saved, the
    API refusing 99%, every version in the security log) and the AML e2e.
  - Left: step-up authentication before saving (module 22), notifying the
    client by email of a change, and designations for business accounts.
- [ ] **26. Tax Center** (W-8BEN, SAT tax withholding certificates, CSV export)
  - W-8BEN applies only to US securities held through a US broker or
    custodian.
- [ ] **27. Corporate Actions UI** (Proxy voting, DRIP elections)
- [x] **28. Cryptographic Solvency Explorer** (Merkle tree leaf node auditor)
  - The Reserve tab (`components/reserve/`), in three parts. The published
    snapshot, for everyone: the reserve root with Copy root, its time, the
    reserve ratio (custody over client liabilities, rounded down so a cent
    short never reads 100.00%), both totals, the cash and market ledger
    anchors, and each asset's root with the units owed to clients. The leaf
    node auditor: the client's leaf hash, what it commits to (cash line by
    line, funds on hold in the neutral copy, and each position at the
    snapshot next to now), a JSON download and the gold "Verify My
    Inclusion". The step-by-step trace, one per tree: every level's two
    children in hashing order with their sums and the node they give, to the
    computed root beside the published one, under the match seal.
  - The backend already served the cash proof
    (`GET /api/v1/reserva/prueba`) and public cuts. Migration 0018 adds the
    positions the request asked to see as included: one Merkle sum tree per
    asset, published with the cut and checked by the database at commit
    (exactly the live open positions, sums, indices, leaf hashes recomputed
    in SQL), plus the market ledger anchor and each cash leaf's breakdown.
    The cash tree and its root are unchanged. README "Proof of Reserve:
    positions and the Solvency Explorer".
  - The verifier, `lib/merkle-verifier.ts` (the former `lib/merkle.ts`):
    WebCrypto SHA-256 with a pure-JS fallback, no dependencies. It
    recomputes every path on a freshly fetched proof, checks the turns
    against the leaf's index, and bounds every sum. The verdict also requires
    the client's own account and the snapshot, asset roots included, equal
    to its public copy. Reference vectors computed separately from the spec
    pin the same hashes in Rust, TypeScript and SQL.
  - Tests: `tests/db/0018_reserva_posiciones.sql`, `backend/tests/reserva.rs`
    and the vectors in `crypto/merkle.rs`, `lib/merkle-verifier.test.ts`,
    `lib/proof-verdict.test.ts`, `lib/solvency.test.ts`, smoke e2e (the
    explorer on fresh snapshots, every position on a phone, a private root
    and another client's leaf refused) and the AML e2e (a frozen client's
    leaf with funds on hold: neutral copy, still verifies).
  - Left: custody attestations for the underlying assets, so an asset's
    root total can be compared with what the custodian holds (today a root
    proves inclusion, not backing); splitting large balances over several
    leaves, so a sibling's sum cannot reveal one client's exact amount; and
    a downloadable standalone verifier page for auditors.
- [ ] **29. Collateral Health Alerts** (Margin monitors and Margin Call countdowns)
  - Gated by module 19 and the margin decision above.

## Phase 6: Engagement

- [ ] **30. Push Notification Center** (Price alerts, volatility warnings)
  - Web Push needs a service worker. iOS delivers it only to an app installed
    on the home screen.
- [ ] **31. Market Data Paywalls** (In-app subscriptions for Nasdaq/CME real-time data)
  - Waits on redistribution agreements with the exchanges.
- [ ] **32. Referral Engine** ("Invite & Earn" gamification and unboxing)
