"use client";

import { useEffect, useRef } from "react";

import { isIdle } from "@/lib/security";

/** What counts as activity in this tab. */
const ACTIVITY_EVENTS = ["pointerdown", "pointermove", "keydown", "wheel", "touchstart", "scroll"] as const;
/** How often the tab checks; the sign-out lands within this much of the timeout. */
const CHECK_EVERY_MS = 10_000;

/**
 * Calls `onIdle` once after `minutes` without a click, key, touch, scroll or
 * pointer move in this tab. A hidden tab counts as idle: when it comes back
 * after the timeout, it signs out at once. Background polling does not count:
 * only the person does.
 *
 * The server keeps its own clock (requests count there), so a closed laptop
 * ends the session on the server too; this one covers an open, unattended
 * tab whose dashboard keeps polling.
 */
export function useIdleSignOut(minutes: number, onIdle: () => void) {
  const latest = useRef(onIdle);
  useEffect(() => {
    latest.current = onIdle;
  });

  useEffect(() => {
    let last = Date.now();
    let fired = false;
    const bump = () => {
      last = Date.now();
    };
    const check = () => {
      if (fired || !isIdle(last, minutes)) return;
      fired = true;
      latest.current();
    };
    const onVisibility = () => {
      if (document.visibilityState === "visible") check();
    };
    for (const name of ACTIVITY_EVENTS) window.addEventListener(name, bump, { passive: true, capture: true });
    document.addEventListener("visibilitychange", onVisibility);
    const timer = setInterval(check, CHECK_EVERY_MS);
    return () => {
      for (const name of ACTIVITY_EVENTS) window.removeEventListener(name, bump, { capture: true });
      document.removeEventListener("visibilitychange", onVisibility);
      clearInterval(timer);
    };
  }, [minutes]);
}
