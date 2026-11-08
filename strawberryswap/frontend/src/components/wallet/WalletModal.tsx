"use client";

import { createContext, useCallback, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { chain, config } from "@/lib/config";
import { proofsAvailable } from "@/zk/availability";
import { useApp } from "@/components/AppState";
import { useSettings } from "@/components/settings/Settings";
import { IconBrowser, IconChip, IconClose, IconCopy, IconLink, IconLock, IconShield } from "@/components/ui/icons";
import { Notice } from "@/components/ui/primitives";

type View = "connect" | "hardware" | "account";

interface WalletModalApi {
  /** Opens the connect sheet, or the account sheet when a wallet is already connected. */
  openWallet: () => void;
}

const WalletModalContext = createContext<WalletModalApi | null>(null);

export function useWalletModal(): WalletModalApi {
  const v = useContext(WalletModalContext);
  if (!v) throw new Error("useWalletModal() must be used inside <WalletModalProvider>");
  return v;
}

export function WalletModalProvider({ children }: { children: ReactNode }) {
  const { wallet } = useApp();
  const [view, setView] = useState<View | null>(null);
  const openWallet = useCallback(() => setView(wallet.account ? "account" : "connect"), [wallet.account]);
  const close = useCallback(() => setView(null), []);
  return (
    <WalletModalContext.Provider value={{ openWallet }}>
      {children}
      {view ? <WalletModal view={view} setView={setView} onClose={close} /> : null}
    </WalletModalContext.Provider>
  );
}

/* ---------------------------------------------------------------------------------------------------------------- */

/**
 * EIP-6963 discovery of the wallet behind `window.ethereum`, for its name and icon. Only inline `data:image/` icons are
 * shown, so an extension cannot make this page fetch anything.
 */
function useInjectedWalletInfo(): { name: string; icon?: string } | undefined {
  const [info, setInfo] = useState<{ name: string; icon?: string }>();
  useEffect(() => {
    const eth = window.ethereum;
    if (!eth) return;
    const seen: { name: string; icon?: string; provider: unknown }[] = [];
    const onAnnounce = (e: Event) => {
      const d = (e as CustomEvent<{ info?: { name?: unknown; icon?: unknown }; provider?: unknown }>).detail;
      if (!d?.info || typeof d.info.name !== "string") return;
      const icon = typeof d.info.icon === "string" && d.info.icon.startsWith("data:image/") ? d.info.icon : undefined;
      seen.push({ name: d.info.name.slice(0, 40), icon, provider: d.provider });
      const exact = seen.find((s) => s.provider === eth);
      const pick = exact ?? (seen.length === 1 ? seen[0] : undefined);
      if (pick) setInfo({ name: pick.name, icon: pick.icon });
    };
    window.addEventListener("eip6963:announceProvider", onAnnounce);
    window.dispatchEvent(new Event("eip6963:requestProvider"));
    return () => window.removeEventListener("eip6963:announceProvider", onAnnounce);
  }, []);
  return info;
}

function Option({
  icon,
  name,
  desc,
  badge,
  onClick,
  disabled,
}: {
  icon: ReactNode;
  name: string;
  desc: string;
  badge?: ReactNode;
  onClick?: () => void;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className="group flex w-full items-center gap-3 rounded-2xl border border-lavender/10 bg-plum-800/70 p-3 text-left transition duration-200 hover:-translate-y-0.5 hover:border-fuchsia hover:bg-plum-700/80 hover:shadow-glow-fuchsia focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-fuchsia disabled:cursor-not-allowed disabled:opacity-60 disabled:hover:translate-y-0 disabled:hover:border-lavender/10 disabled:hover:bg-plum-800/70 disabled:hover:shadow-none"
    >
      <span className="grid h-12 w-12 shrink-0 place-items-center overflow-hidden rounded-2xl bg-plum-700 text-lavender shadow-inset transition group-hover:bg-fuchsia-deep group-hover:text-white group-disabled:bg-plum-700 group-disabled:text-lavender">
        {icon}
      </span>
      <span className="min-w-0 flex-1">
        <span className="flex flex-wrap items-center gap-2">
          <span className="font-bold text-ink">{name}</span>
          {badge}
        </span>
        <span className="mt-0.5 block text-xs leading-snug text-lavender">{desc}</span>
      </span>
    </button>
  );
}

const FOCUSABLE = 'button:not([disabled]), [href], input:not([disabled]), [tabindex]:not([tabindex="-1"])';

function WalletModal({ view, setView, onClose }: { view: View; setView: (v: View) => void; onClose: () => void }) {
  const { wallet, ready, shielded } = useApp();
  const { t } = useSettings();
  const injected = useInjectedWalletInfo();
  const dialog = useRef<HTMLDivElement>(null);
  const closeRef = useRef(onClose);
  closeRef.current = onClose;
  const [pendingShielded, setPendingShielded] = useState(false);
  const [busy, setBusy] = useState(false);
  const [copied, setCopied] = useState(false);
  const shieldedAvailable = !!config.darkPool && proofsAvailable;

  // Modal mechanics: Esc closes, Tab stays inside, the page behind does not scroll, focus returns to the opener.
  useEffect(() => {
    const opener = document.activeElement as HTMLElement | null;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        closeRef.current();
        return;
      }
      if (e.key !== "Tab" || !dialog.current) return;
      const f = Array.from(dialog.current.querySelectorAll<HTMLElement>(FOCUSABLE));
      const first = f.at(0);
      const last = f.at(-1);
      if (!first || !last) return;
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", onKey);
    const overflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      document.removeEventListener("keydown", onKey);
      document.body.style.overflow = overflow;
      opener?.focus?.();
    };
  }, []);

  useEffect(() => {
    dialog.current?.querySelector<HTMLElement>(FOCUSABLE)?.focus();
  }, [view]);

  // A plain connection closes the sheet as soon as an account appears.
  useEffect(() => {
    if ((view === "connect" || view === "hardware") && wallet.account && !pendingShielded) onClose();
  }, [view, wallet.account, pendingShielded, onClose]);

  // Shielded Wallet: once the account is connected on the right chain, unlock (one signature), then close.
  useEffect(() => {
    if (!pendingShielded || !ready) return;
    if (shielded.phase === "locked" || shielded.phase === "error") void shielded.unlock();
    setPendingShielded(false);
    onClose();
  }, [pendingShielded, ready, shielded, onClose]);

  const connect = async () => {
    setBusy(true);
    try {
      await wallet.connect();
    } finally {
      setBusy(false);
    }
  };

  const startShielded = () => {
    setPendingShielded(true);
    if (!wallet.account) void connect();
  };

  const copy = async () => {
    if (!wallet.account) return;
    try {
      await navigator.clipboard.writeText(wallet.account);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      /* clipboard blocked: the address stays selectable below */
    }
  };

  const title = view === "account" ? t("modal.account.title") : view === "hardware" ? t("modal.hardware.name") : t("modal.connect.title");
  const phasePill = {
    locked: <span className="pill-plum">{t("phase.locked")}</span>,
    unlocking: <span className="pill-blush">{t("phase.unlocking")}</span>,
    syncing: <span className="pill-blush">{t("phase.syncing")}</span>,
    ready: <span className="pill-mint">{t("phase.ready")}</span>,
    error: <span className="pill-berry">{t("phase.error")}</span>,
  }[shielded.phase];

  const sheet = (
    <div className="fixed inset-0 z-[60] flex animate-fade-in items-end justify-center bg-plum-950/70 p-0 backdrop-blur-md sm:items-center sm:p-4" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div
        ref={dialog}
        role="dialog"
        aria-modal="true"
        aria-labelledby="wallet-modal-title"
        className="w-full max-w-md animate-sheet-up rounded-t-3xl border border-lavender/15 bg-plum-900/90 p-5 shadow-card backdrop-blur-xl sm:rounded-3xl sm:p-6"
        style={{ paddingBottom: "max(1.25rem, env(safe-area-inset-bottom))" }}
      >
        <header className="mb-5 flex items-start justify-between gap-3">
          <div>
            <h2 id="wallet-modal-title" className="text-xl font-extrabold tracking-tight">{title}</h2>
            {view === "connect" ? <p className="mt-0.5 text-sm text-lavender">{t("modal.connect.subtitle")}</p> : null}
          </div>
          <button type="button" onClick={onClose} aria-label={t("modal.close")} className="grid h-9 w-9 shrink-0 place-items-center rounded-full bg-plum-700/80 text-lavender transition hover:bg-fuchsia/20 hover:text-ink focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-fuchsia">
            <IconClose className="h-4 w-4" />
          </button>
        </header>

        {view === "connect" ? (
          <div className="space-y-2.5">
            <Option
              icon={injected?.icon ? <img src={injected.icon} alt="" width={30} height={30} /> : <IconBrowser />}
              name={injected?.name ?? t("modal.browser.name")}
              desc={t("modal.browser.desc")}
              badge={wallet.hasProvider ? <span className="pill-mint">{t("modal.browser.detected")}</span> : <span className="pill-plum">{t("modal.browser.missing")}</span>}
              onClick={() => void connect()}
              disabled={!wallet.hasProvider || busy}
            />
            <Option
              icon={<IconShield />}
              name={t("modal.shielded.name")}
              desc={t("modal.shielded.desc")}
              badge={shieldedAvailable ? <span className="pill-berry">{t("modal.shielded.badge")}</span> : <span className="pill-plum">{t("modal.shielded.unavailable")}</span>}
              onClick={startShielded}
              disabled={!shieldedAvailable || !wallet.hasProvider || busy}
            />
            <Option icon={<IconChip />} name={t("modal.hardware.name")} desc={t("modal.hardware.desc")} onClick={() => setView("hardware")} disabled={!wallet.hasProvider} />
            <Option icon={<IconLink />} name={t("modal.walletconnect.name")} desc={t("modal.walletconnect.desc")} badge={<span className="pill-plum">{t("modal.walletconnect.badge")}</span>} disabled />
          </div>
        ) : null}

        {view === "hardware" ? (
          <div>
            <ol className="space-y-2.5">
              {(["modal.hardware.step1", "modal.hardware.step2", "modal.hardware.step3"] as const).map((k, i) => (
                <li key={k} className="card-inset flex gap-3 text-sm text-lavender">
                  <span className="grid h-7 w-7 shrink-0 place-items-center rounded-full bg-plum-700 text-xs font-bold text-ink ring-1 ring-lavender/15">{i + 1}</span>
                  {t(k)}
                </li>
              ))}
            </ol>
            <div className="mt-4 flex gap-2">
              <button type="button" className="btn-ghost" onClick={() => setView("connect")}>{t("modal.back")}</button>
              <button type="button" className="btn-primary flex-1" disabled={busy || !wallet.hasProvider} onClick={() => void connect()}>
                <IconChip className="h-5 w-5" />
                {t("modal.hardware.continue")}
              </button>
            </div>
          </div>
        ) : null}

        {view === "account" && wallet.account ? (
          <div className="space-y-3">
            <div className="card-inset">
              <div className="flex items-center justify-between gap-3">
                <span className="flex items-center gap-2 text-sm text-lavender">
                  <span className={`h-2.5 w-2.5 rounded-full ${wallet.chainOk ? "bg-mint shadow-glow-mint" : "bg-blush"}`} aria-hidden />
                  {t("modal.account.network")}: <span className="num text-ink">{chain.name} · {chain.id}</span>
                </span>
                {!wallet.chainOk ? (
                  <button type="button" className="btn-secondary !px-3 !py-1.5 text-xs" onClick={() => void wallet.switchChain()}>{t("header.switch")}</button>
                ) : null}
              </div>
              <p className="num mt-3 select-all break-all text-sm text-ink">{wallet.account}</p>
              <button type="button" className="btn-secondary mt-3 !py-1.5 text-xs" onClick={() => void copy()}>
                <IconCopy className="h-4 w-4" />
                {copied ? t("modal.account.copied") : t("modal.account.copy")}
              </button>
            </div>
            <div className="card-inset flex items-center justify-between gap-3">
              <span className="flex items-center gap-2 text-sm">
                <IconShield className="h-4 w-4 text-lavender" />
                {t("modal.account.shielded")} {phasePill}
              </span>
              {shielded.phase === "ready" || shielded.phase === "syncing" ? (
                <button type="button" className="btn-ghost !py-1.5 text-xs" onClick={() => void shielded.lock()}>
                  <IconLock className="h-4 w-4" />
                  {t("modal.account.lock")}
                </button>
              ) : shielded.phase === "locked" || shielded.phase === "error" ? (
                <button type="button" className="btn-primary !px-4 !py-1.5 text-xs" disabled={!shieldedAvailable || !ready} onClick={() => { void shielded.unlock(); onClose(); }}>
                  {t("modal.account.unlock")}
                </button>
              ) : null}
            </div>
            <p className="px-1 text-xs text-haze">{t("modal.account.disconnect")}</p>
          </div>
        ) : null}

        {pendingShielded && wallet.account && !wallet.chainOk ? (
          <div className="mt-4 flex items-center justify-between gap-3 rounded-2xl border border-blush/30 bg-blush/10 px-4 py-3 text-sm text-blush">
            {t("modal.switchFirst", { chain: chain.name })}
            <button type="button" className="btn-secondary !px-3 !py-1.5 text-xs" onClick={() => void wallet.switchChain()}>{t("header.switch")}</button>
          </div>
        ) : null}
        {busy ? <p className="mt-4 text-center text-sm text-lavender" aria-live="polite">{t("modal.waiting")}</p> : null}
        {wallet.error ? <Notice tone="err" className="mt-4">{wallet.error}</Notice> : null}
        {view !== "account" ? <p className="mt-5 border-t border-lavender/10 pt-4 text-center text-xs text-haze">{t("modal.footer")}</p> : null}
      </div>
    </div>
  );
  return createPortal(sheet, document.body);
}
