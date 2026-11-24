/**
 * Step-up authentication (migration 0022): before money leaves the account
 * or the beneficiaries change, the session confirms a credential again.
 */

/** How this session confirms: the authenticator code with two-step verification on, else the password. */
export type StepUpMethod = "totp" | "password";

/** GET /api/v1/auth/step-up. */
export interface StepUpStatus {
  method: StepUpMethod;
  /** End of the verified window, if one is open. */
  validUntil: string | null;
  /** Seconds left in it, by the server's clock (0 without one). */
  remainingSeconds: number;
  /** How long a new window lasts (300). */
  windowSeconds: number;
}

/** POST /api/v1/auth/step-up: the window just opened. */
export interface StepUpWindow {
  method: StepUpMethod;
  validUntil: string;
  /** 300, or less when the session ends sooner. */
  seconds: number;
}

/** What the person typed. */
export type StepUpCredential = { method: "totp"; code: string } | { method: "password"; password: string };

/** The action waiting behind the check, as the modal names it. */
export interface StepUpRequest {
  /** "Withdraw $1,000.00", "Save your beneficiaries". */
  action: string;
  /** A second line: where the money goes, how many people. */
  detail?: string | null;
  /** The error the action shows if the person closes the check: "Nothing was sent." */
  cancelled: string;
}
