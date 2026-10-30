/**
 * Two-step verification (TOTP) and password recovery helpers, kept free of
 * React so `node --test` can check them.
 */

/** Digits of an authenticator code: the code is typed or pasted as "123 456" or "123-456". */
export const TOTP_DIGITS = 6;

export function normalizeTotpInput(input: string): string {
  return input.replace(/\D/g, "").slice(0, TOTP_DIGITS);
}

export function isTotpCode(code: string): boolean {
  return new RegExp(`^\\d{${TOTP_DIGITS}}$`).test(code);
}

/**
 * The reset token from the link's fragment (`#token=...`). Browsers never send
 * the fragment to a server, so the token stays out of access logs and the
 * Referer header. Null when absent or malformed.
 */
export function resetTokenFromHash(hash: string): string | null {
  const params = new URLSearchParams(hash.startsWith("#") ? hash.slice(1) : hash);
  const token = params.get("token");
  return token && /^[A-Za-z0-9_-]{20,128}$/.test(token) ? token : null;
}

/** The backend's password policy (backend/src/auth/password.rs). */
export const PASSWORD_MIN_LENGTH = 12;
export const PASSWORD_MAX_LENGTH = 128;

/** Why a new password cannot be submitted yet, or null when it can. */
export function newPasswordProblem(password: string, confirmation: string): string | null {
  const length = [...password].length;
  if (length < PASSWORD_MIN_LENGTH) return `Use at least ${PASSWORD_MIN_LENGTH} characters.`;
  if (length > PASSWORD_MAX_LENGTH) return `Use at most ${PASSWORD_MAX_LENGTH} characters.`;
  if (password !== confirmation) return "The two passwords do not match.";
  return null;
}

/** A base32 secret in groups of four, easier to type into an authenticator app by hand. */
export function formatSecretForManualEntry(secret: string): string {
  return (secret.replace(/\s/g, "").match(/.{1,4}/g) ?? []).join(" ");
}
