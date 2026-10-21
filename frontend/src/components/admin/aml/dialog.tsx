"use client";

import { useEffect, useId, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { X } from "lucide-react";

import { cx } from "@/components/ui/primitives";

const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]):not([type="hidden"]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/** Dialogs open at once (a confirmation on top of the case file): the page scroll stays locked until the last closes. */
let openDialogs = 0;

/**
 * Modal dialog (`variant="modal"`, centered) or side drawer (`variant="drawer"`):
 * role="dialog", aria-modal, focus trapped inside, Esc closes the topmost one,
 * and focus returns to the element that opened it. Rendered in a portal so a
 * blurred sticky header can never become its containing block.
 */
export function Dialog({
  open,
  onClose,
  title,
  description,
  children,
  footer,
  variant = "modal",
  className,
  busy = false,
}: {
  open: boolean;
  onClose: () => void;
  title: ReactNode;
  description?: ReactNode;
  children?: ReactNode;
  footer?: ReactNode;
  variant?: "modal" | "drawer";
  className?: string;
  /** While a request is in flight, Esc and the backdrop do not close the dialog. */
  busy?: boolean;
}) {
  const [mounted, setMounted] = useState(false);
  const panelRef = useRef<HTMLDivElement>(null);
  const titleId = useId();
  const descriptionId = useId();
  const closeRef = useRef(onClose);
  const busyRef = useRef(busy);

  useEffect(() => {
    closeRef.current = onClose;
    busyRef.current = busy;
  }, [onClose, busy]);

  useEffect(() => setMounted(true), []);

  useEffect(() => {
    if (!open || !mounted) return;
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const panel = panelRef.current;
    const first = panel?.querySelector<HTMLElement>("[data-autofocus]") ?? panel?.querySelector<HTMLElement>(FOCUSABLE);
    (first ?? panel)?.focus();
    openDialogs += 1;
    document.body.style.overflow = "hidden";
    return () => {
      openDialogs -= 1;
      if (openDialogs === 0) document.body.style.overflow = "";
      if (opener?.isConnected) opener.focus();
    };
  }, [open, mounted]);

  if (!open || !mounted) return null;

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Escape") {
      // Only the topmost dialog handles it: events from a nested dialog's portal bubble through this one.
      event.stopPropagation();
      if (!busyRef.current) closeRef.current();
      return;
    }
    if (event.key !== "Tab" || !panelRef.current) return;
    event.stopPropagation();
    const items = Array.from(panelRef.current.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
      (el) => el.offsetParent !== null || el === document.activeElement,
    );
    if (items.length === 0) {
      event.preventDefault();
      panelRef.current.focus();
      return;
    }
    const firstItem = items[0]!;
    const lastItem = items[items.length - 1]!;
    const active = document.activeElement;
    const inside = active instanceof Node && panelRef.current.contains(active);
    if (event.shiftKey && (active === firstItem || !inside)) {
      event.preventDefault();
      lastItem.focus();
    } else if (!event.shiftKey && (active === lastItem || !inside)) {
      event.preventDefault();
      firstItem.focus();
    }
  };

  const drawer = variant === "drawer";
  return createPortal(
    <div
      className={cx("fixed inset-0 z-50 flex bg-black/40 backdrop-blur-[2px] dark:bg-black/60", drawer ? "justify-end" : "items-center justify-center p-4")}
      onMouseDown={(event) => {
        // A click that starts on the backdrop itself (not one bubbling from a nested dialog) closes a drawer.
        if (drawer && event.target === event.currentTarget && !busyRef.current) closeRef.current();
      }}
    >
      <div
        ref={panelRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={description ? descriptionId : undefined}
        tabIndex={-1}
        onKeyDown={onKeyDown}
        className={cx(
          "flex max-h-full flex-col border-border bg-surface shadow-xl outline-none",
          drawer ? "h-full w-full max-w-4xl border-l" : "max-h-[calc(100dvh-2rem)] w-full max-w-lg rounded-xl border",
          className,
        )}
      >
        <header className="flex items-start justify-between gap-4 border-b border-border px-5 py-4">
          <div className="min-w-0 space-y-1">
            <h2 id={titleId} className="text-base font-semibold text-ink">
              {title}
            </h2>
            {description ? (
              <div id={descriptionId} className="text-sm leading-relaxed text-ink-2">
                {description}
              </div>
            ) : null}
          </div>
          <button
            type="button"
            onClick={() => !busy && onClose()}
            className="shrink-0 rounded-md p-1 text-muted transition hover:bg-surface-2 hover:text-ink focus-visible:ring-2 focus-visible:ring-accent focus-visible:outline-none"
            aria-label="Close"
          >
            <X className="size-4" aria-hidden />
          </button>
        </header>
        <div className="min-h-0 flex-1 overflow-y-auto">{children}</div>
        {footer ? (
          <footer className="flex flex-wrap items-center justify-end gap-2 border-t border-border px-5 py-3">{footer}</footer>
        ) : null}
      </div>
    </div>,
    document.body,
  );
}
