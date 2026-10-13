/** Base URL of the Rust backend (axum). Configure with NEXT_PUBLIC_API_URL. */
export const API_URL = (process.env.NEXT_PUBLIC_API_URL ?? "http://localhost:8080").replace(/\/+$/, "");

/** How often the dashboard refreshes balances while the tab is visible. */
export const POLL_INTERVAL_MS = 15_000;
