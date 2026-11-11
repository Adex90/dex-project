/**
 * Bóveda Soberana (Sovereign Vault): the data contract of the client's
 * dashboard, exactly as the backend sends it.
 *
 * - Field names stay snake_case, as they travel on the wire, so a response
 *   needs no mapping layer between the network and the store.
 * - Money, prices, share quantities and percentages are decimal strings
 *   ("15000.00", "0.25", "-1.40"), the way the backend serializes
 *   rust_decimal. They are read and formatted with `src/lib/decimal.ts`
 *   (BigInt, at most 8 decimals) and never pass through floating point.
 * - Nothing from the network is trusted as typed: `src/lib/vault.ts`
 *   validates REST snapshots and WebSocket messages before they reach the
 *   store, and returns fresh objects holding only the fields below.
 */

/** A decimal number serialized as a string: "15000.00", "-1.25". At most 8 decimals. */
export type DecimalString = string;

/* ------------------------------------------------------------------------ */
/* Snapshot (REST)                                                          */
/* ------------------------------------------------------------------------ */

/** The vault's totals, in MXN. */
export interface VaultBalance {
  /**
   * Net worth: cash (available, held for open orders and on hold) plus
   * `invested_assets`. With funds on hold, `available_cash` + `held_cash` +
   * `invested_assets` is less than the total.
   */
  total_mxn: DecimalString;
  /** Settled cash, free to withdraw or invest right now. */
  available_cash: DecimalString;
  /**
   * Cash committed to pending limit and stop buys (migration 0015): still the
   * client's and in custody, but not spendable until the order fills or is
   * cancelled. "0" from a backend that predates it.
   */
  held_cash: DecimalString;
  /** Market value of every holding at its current price. */
  invested_assets: DecimalString;
  /** Unrealized profit or loss on the invested assets, in percent: "4.25" is +4.25 %. */
  unrealized_pnl_percentage: DecimalString;
}

/**
 * State of the latest proof of reserve. The backend commits the root of the
 * client-liabilities tree through the ZK rollup and proves that custody
 * covers it 1:1.
 *
 * - `verified`: the latest proof checked out: reserves cover 100 % of deposits.
 * - `pending`: a new epoch is being proven; the last verified root still stands.
 * - `failed`: the latest proof did not verify. The UI must never show "verified".
 */
export const RESERVE_PROOF_STATUSES = ["verified", "pending", "failed"] as const;
export type ReserveProofStatus = (typeof RESERVE_PROOF_STATUSES)[number];

export interface ReserveProof {
  status: ReserveProofStatus;
  /** Root hash of the latest proven reserve state, hex (optionally 0x-prefixed). */
  zk_root_hash: string;
}

/** One position in the client's portfolio. */
export interface Holding {
  /** Exchange symbol, unique within the portfolio: "NVDA", "CETES28". */
  ticker: string;
  name: string;
  /** Units held, those committed to pending sells included; fractional shares allowed. */
  shares: DecimalString;
  /**
   * Units committed to pending limit and stop sells: a market sale only sells
   * `shares` − `held_shares`. "0" from a backend that predates it.
   */
  held_shares: DecimalString;
  /** Last price per share, MXN. */
  current_price: DecimalString;
  /** `shares` × `current_price`, computed by the backend, MXN. */
  total_value: DecimalString;
  /** Price change since the previous close, in percent. */
  daily_change_pct: DecimalString;
}

/** The client's whole vault: the body of the vault endpoint. */
export interface VaultSnapshot {
  user_id: string;
  /** The 18-digit CLABE where the client receives SPEI transfers. */
  clabe: string;
  /** The institution that holds the CLABE, as the sender's bank shows it. */
  bank_name: string;
  balance: VaultBalance;
  reserve_proof: ReserveProof;
  holdings: Holding[];
}

/** Who owns the vault and where it receives SPEI transfers. */
export type VaultAccount = Pick<VaultSnapshot, "user_id" | "clabe" | "bank_name">;

/* ------------------------------------------------------------------------ */
/* Real-time channel (WebSocket)                                            */
/* ------------------------------------------------------------------------ */

export const SPEI_DIRECTIONS = ["inbound", "outbound"] as const;
export type SpeiDirection = (typeof SPEI_DIRECTIONS)[number];

/**
 * - `settled`: credited (inbound) or sent (outbound); the message's balance includes it.
 * - `returned`: refused or returned (devolución); the message's balance reflects that.
 */
export const SPEI_TRANSFER_STATUSES = ["settled", "returned"] as const;
export type SpeiTransferStatus = (typeof SPEI_TRANSFER_STATUSES)[number];

/** A SPEI transfer the backend has applied to the vault. */
export interface SpeiTransfer {
  /** SPEI tracking key (clave de rastreo), set by the sending bank. */
  tracking_key: string;
  direction: SpeiDirection;
  status: SpeiTransferStatus;
  /** Always positive; `direction` says which way it moved. */
  amount: DecimalString;
  /** The sender of an inbound transfer, the beneficiary of an outbound one. */
  counterparty_name: string;
  /**
   * The counterparty's bank as its 3-digit code, the CLABE prefix ("012" is
   * BBVA México, "646" STP). No name catalog yet.
   */
  counterparty_bank: string;
  /** When the backend applied it, ISO 8601. */
  occurred_at: string;
}

/**
 * Server-to-client messages. Each carries `seq`, the version of the account
 * whose values it brings (the backend's `ifpe.cuentas.seq`: +1 per
 * transaction that changes a balance).
 *
 * The protocol is snapshot-first: on every (re)connection the server sends a
 * `vault.snapshot`, then deltas. A delta carries the authoritative values
 * after the change (the new balance, the whole holding), never increments,
 * so the client does no money arithmetic and replaying a message is harmless.
 *
 * - A snapshot may repeat the current `seq`: that is the answer to a
 *   `{"type": "resync"}`, or a new reserve proof (a cut changes no balance).
 *   The backend sends new proofs that way rather than as `reserve.updated`.
 * - `holding.updated` with `shares` "0" means the position was closed.
 */
export type VaultRealtimeMessage =
  | { type: "vault.snapshot"; seq: number; vault: VaultSnapshot }
  | { type: "spei.transfer"; seq: number; transfer: SpeiTransfer; balance: VaultBalance }
  | { type: "holding.updated"; seq: number; holding: Holding; balance: VaultBalance }
  | { type: "reserve.updated"; seq: number; reserve_proof: ReserveProof };

export type VaultRealtimeMessageType = VaultRealtimeMessage["type"];
