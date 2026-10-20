/** Base URL of the Rust backend (axum). Configure with NEXT_PUBLIC_API_URL. */
export const API_URL = (process.env.NEXT_PUBLIC_API_URL ?? "http://localhost:8080").replace(/\/+$/, "");

/** How often the dashboard refreshes balances while the tab is visible. */
export const POLL_INTERVAL_MS = 15_000;

/**
 * The one channel customers are pointed to for compliance matters (rejected,
 * suspended or locked files, identity conflicts, privacy notice, ARCO rights
 * and withdrawing biometric consent). Configure with NEXT_PUBLIC_COMPLIANCE_EMAIL;
 * the default is a reserved placeholder domain, not a real mailbox.
 */
export const COMPLIANCE_EMAIL = process.env.NEXT_PUBLIC_COMPLIANCE_EMAIL ?? "compliance@synthetic-capital.example";
