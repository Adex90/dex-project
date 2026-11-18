/**
 * Copies text, resolving to whether it worked. Uses the async Clipboard API
 * where the page is a secure context, and falls back to a hidden textarea
 * where it is not (plain-HTTP previews, some in-app browsers) or where the
 * browser refuses permission. Never throws.
 */
export async function writeToClipboard(text: string): Promise<boolean> {
  if (window.isSecureContext && navigator.clipboard) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      // Permission denied or no user activation: try the legacy path.
    }
  }
  return legacyCopy(text);
}

function legacyCopy(text: string): boolean {
  const previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const field = document.createElement("textarea");
  field.value = text;
  field.setAttribute("readonly", "");
  // Fixed and transparent: no layout shift, no scroll jump, nothing visible.
  Object.assign(field.style, { position: "fixed", top: "0", left: "0", opacity: "0", pointerEvents: "none" });
  document.body.append(field);
  field.select();
  field.setSelectionRange(0, text.length); // iOS Safari ignores select() alone.
  try {
    return document.execCommand("copy");
  } catch {
    return false;
  } finally {
    field.remove();
    previousFocus?.focus(); // Give keyboard users their place back.
  }
}
