/**
 * CLABE (Clave Bancaria Estandarizada): the 18-digit account number SPEI
 * routes to. Bank (3) · plaza (3) · account (11) · check digit (1).
 *
 * The check digit follows Banxico's rule, the same one the backend applies
 * (backend/src/spei/clabe.rs): multiply each of the first 17 digits by the
 * weights 3, 7, 1, 3, 7, 1, …, keep each product mod 10, add them up, and
 * the check digit is (10 − sum mod 10) mod 10.
 */

const CLABE_RE = /^\d{18}$/;
const WEIGHTS = [3, 7, 1];

/** Digit at `index` of a string already known to be all digits. */
function digitAt(value: string, index: number): number {
  return value.charCodeAt(index) - 48;
}

/** True for an 18-digit CLABE whose check digit is correct. */
export function isValidClabe(clabe: string): boolean {
  if (!CLABE_RE.test(clabe)) return false;
  let sum = 0;
  for (let i = 0; i < 17; i++) sum += (digitAt(clabe, i) * (WEIGHTS[i % 3] ?? 0)) % 10;
  return (10 - (sum % 10)) % 10 === digitAt(clabe, 17);
}

/**
 * Groups a CLABE the way banks print it: "032180000118359719" becomes
 * "032 180 00011835971 9". Anything that is not 18 digits comes back
 * unchanged, so a bad value is never disguised as a well-formed one.
 */
export function formatClabe(clabe: string): string {
  if (!CLABE_RE.test(clabe)) return clabe;
  return `${clabe.slice(0, 3)} ${clabe.slice(3, 6)} ${clabe.slice(6, 17)} ${clabe.slice(17)}`;
}
