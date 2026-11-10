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
    the current design yield comes only from SOFOM contracts, which module 13
    must respect.
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
  - `components/markets/asset-detail.tsx`, with the price chart on the shared
    `components/charts/trend-chart.tsx` (the vault's balance chart uses it too).
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
- [ ] **3. Advanced Order Engine** (Limit, Stop-Loss, Trailing, Day/GTC)
  - Builds on module 2.
- [ ] **4. Savings Plans / DCA** (Automated fractional recurring buys)
  - Waits on recurring-order scheduling and fractional shares in the backend.
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
  - Left: top movers and collections (curated lists), and predictive search
    once the catalog outgrows a single page (a `q` parameter on the
    endpoint).
- [ ] **9. Asset Screener** (Cross-filtering technicals/fundamentals)
- [ ] **10. Pro Charting** (TradingView wrapper, candlesticks, indicators, drawing tools)
  - TradingView's Charting Library needs a license; Lightweight Charts is open
    source. Either way, self-host it: the CSP allows only the app's own scripts.
- [ ] **11. Portfolio Analytics** (Donut charts, sector/geo distribution, dividend tracker)
  - Foundation: the Vault tab's asset list (`components/vault/asset-list.tsx`)
    and the cash chart.
- [ ] **12. Corporate Events Calendar** (Earnings, ex-dividend dates)

## Phase 3: Wealth & Liquidity

- [ ] **13. Cash/Yield Tab** (Uninvested balance and live interest counter)
  - Foundation: the Vault and Account tabs. The counter shows SOFOM contract
    accrual, not interest on IFPE cash (see the licensing scope above).
- [ ] **14. Card Management** (Virtual card UI, freeze, PIN, Apple/Google Wallet)
  - Waits on a card issuing processor. Adding a card to Apple or Google
    Wallet from inside the app needs native apps; a web app cannot do it.
- [ ] **15. Round-up & Saveback** (Cashback tracking and spare change investing)
- [ ] **16. Linked Accounts & Withdrawals** (Whitelisting CLABE and fiat off-ramp)
  - The backend already sends withdrawals to an own external CLABE
    (`POST /api/v1/ifpe/retiros`). CLABE check-digit validation exists in
    `lib/clabe.ts`. No UI yet.
- [ ] **17. SPEI Dispute Tracker** (Banxico CEP integration for delayed transfers)
  - Orders already carry their tracking key (`clave_rastreo`). The lookup of
    Banxico's CEP (electronic payment receipt) goes through the backend.

## Phase 4: Identity, Security & KYC

- [ ] **18. AML/KYC Onboarding** (ID scan, liveness check, contract signing)
  - Foundation: `/auth/onboarding` (personal data, INE photos, consent and
    liveness, verdict). Contract signing is missing.
- [ ] **19. Investor Profiling & Quizzes** (Gatekeeping Margin/Options tiers)
  - Gates modules 5 and 29.
- [ ] **20. Security Center** (FaceID/TouchID, PIN reset)
  - On the web, Face ID and Touch ID mean WebAuthn passkeys. TOTP two-step
    verification exists (`/auth/mfa-setup`) but is not linked from the
    dashboard.
- [ ] **21. Advanced Session Management** (Active devices, remote logout)
  - Today the backend can only revoke all of a person's sessions at once
    (operators only). A device list needs per-session records.
- [ ] **22. Step-Up Authentication** (Forced biometrics on anomalous withdrawals)
  - Builds on modules 20 and 16. The anomaly signal comes from the backend's
    AML engine.

## Phase 5: Compliance & Transparency

- [ ] **23. Activity Feed** (Immutable ledger of deposits, trades, and dividends)
  - Foundation: the Account tab's SHA-256-chained ledger feed. Trades and
    dividends arrive with Phase 1.
- [ ] **24. Document Inbox** (PDF statements, trade confirmations)
- [ ] **25. Legal Beneficiaries** (Assigning heirs and succession percentages)
- [ ] **26. Tax Center** (W-8BEN, SAT tax withholding certificates, CSV export)
  - W-8BEN applies only to US securities held through a US broker or
    custodian.
- [ ] **27. Corporate Actions UI** (Proxy voting, DRIP elections)
- [ ] **28. Cryptographic Solvency Explorer** (Merkle tree leaf node auditor)
  - Foundation: the Reserve tab rebuilds the client's Merkle proof in the
    browser (`lib/merkle.ts`). The published cuts are public
    (`/api/v1/reserva/cortes`).
- [ ] **29. Collateral Health Alerts** (Margin monitors and Margin Call countdowns)
  - Gated by module 19 and the margin decision above.

## Phase 6: Engagement

- [ ] **30. Push Notification Center** (Price alerts, volatility warnings)
  - Web Push needs a service worker. iOS delivers it only to an app installed
    on the home screen.
- [ ] **31. Market Data Paywalls** (In-app subscriptions for Nasdaq/CME real-time data)
  - Waits on redistribution agreements with the exchanges.
- [ ] **32. Referral Engine** ("Invite & Earn" gamification and unboxing)
