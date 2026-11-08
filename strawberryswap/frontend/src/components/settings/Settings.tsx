"use client";

import { createContext, useCallback, useContext, useEffect, useId, useMemo, useRef, useState, type ReactNode } from "react";
import { IconGlobe } from "@/components/ui/icons";
import { Segmented } from "@/components/ui/primitives";

export type Lang = "en" | "es";
export type Fiat = "USD" | "EUR" | "MXN";

const LANGS: readonly { value: Lang; label: string }[] = [
  { value: "en", label: "English" },
  { value: "es", label: "Español" },
];
const FIATS: readonly Fiat[] = ["USD", "EUR", "MXN"];

/**
 * Shell strings (navigation, header, dialogs, page headings). Trading screens keep English copy for now; every key
 * must exist in both languages (the type below enforces it).
 */
const EN = {
  "nav.trade": "Trade",
  "nav.stocks": "Stocks & RWAs",
  "nav.stocks.short": "Stocks",
  "nav.earn": "Earn & Vaults",
  "nav.earn.short": "Earn",
  "nav.portfolio": "Portfolio",
  "header.connect": "Connect Wallet",
  "header.noWallet": "No wallet",
  "header.switch": "Switch network",
  "header.wrongNetwork": "Wrong network. Market data stays live; transactions are disabled until the wallet switches to {chain}.",
  "footer.disclaimer": "Unaudited software. Testnet use only. Counterparty is the smart contract, not a firm.",
  "footer.tagline": "100% reserve. Zero inflation. Private by design.",
  "footer.developers": "Developers",
  "footer.docs": "Developer Docs",
  "footer.security": "Security",
  "footer.audits": "Audits",
  "footer.auditsNote": "Internal review published; external audit pending.",
  "footer.company": "Company",
  "footer.terms": "Terms of Service",
  "footer.support": "Support",
  "footer.noEmissions": "No farms · no lotteries · no token emissions",
  "terms.title": "Terms of Service",
  "terms.body1": "The Terms of Service will be published on this page before StrawberrySwap's public launch.",
  "terms.body2": "Until then, this build is pre-launch software for testnet evaluation only.",
  "terms.support": "Questions? Open an issue on GitHub:",
  "terms.back": "Back to the app",
  "settings.title": "Settings",
  "settings.language": "Language",
  "settings.fiat": "Reference currency",
  "settings.fiatNote": "Fiat values use real rates only: USD through the pool's USD stablecoin. EUR and MXN need a rate feed, which this build does not have.",
  "settings.i18nNote": "Navigation, dialogs and page headings follow this language; trading screens are in English for now.",
  "settings.button": "Settings: {lang}, {fiat}",
  "fiat.peg": "at 1 {stable} = 1 USD",
  "fiat.noRate": "No {fiat} rate feed in this build",
  "fiat.noStable": "Needs a USD stablecoin in the pool",
  "page.trade.title": "Trade",
  "page.trade.subtitle": "Private swaps settle in batches at one uniform price. No retail fee; proofs are generated on this device.",
  "page.stocks.title": "Stocks & RWAs",
  "page.stocks.subtitle": "Synthetic exposure to real-world prices, fully collateralised and settled against zero-knowledge-attested quotes.",
  "page.earn.title": "Earn & Vaults",
  "page.earn.subtitle": "Non-custodial strategy vaults and segregated bank deposits. Nobody can withdraw your assets but you.",
  "page.portfolio.title": "Portfolio",
  "page.portfolio.subtitle": "Your shielded balances, vault position and private swap history.",
  "modal.connect.title": "Connect a wallet",
  "modal.connect.subtitle": "StrawberrySwap never sees your private keys.",
  "modal.close": "Close",
  "modal.browser.name": "Browser wallet",
  "modal.browser.desc": "Use the wallet extension in this browser.",
  "modal.browser.detected": "Detected",
  "modal.browser.missing": "Not installed",
  "modal.shielded.name": "Shielded Wallet",
  "modal.shielded.desc": "Connect, then unlock private balances with one signature. Proofs run on this device.",
  "modal.shielded.badge": "Native · ZK",
  "modal.shielded.unavailable": "Not in this build",
  "modal.hardware.name": "Hardware signer",
  "modal.hardware.desc": "Ledger, Trezor or GridPlus, through your browser wallet.",
  "modal.hardware.step1": "Plug in and unlock the device, and open its Ethereum app.",
  "modal.hardware.step2": "In your browser wallet (MetaMask, Rabby, Frame…), add the hardware account and select it.",
  "modal.hardware.step3": "Continue below: every signature is confirmed on the device itself.",
  "modal.hardware.continue": "Continue with browser wallet",
  "modal.walletconnect.name": "WalletConnect",
  "modal.walletconnect.desc": "Pairs a mobile wallet through WalletConnect's relay network, a third party this IPFS build deliberately does not contact.",
  "modal.walletconnect.badge": "Not in this build",
  "modal.back": "Back",
  "modal.waiting": "Confirm in your wallet…",
  "modal.switchFirst": "Switch your wallet to {chain} to unlock.",
  "modal.footer": "Shielded keys are derived on this device and never leave it.",
  "modal.account.title": "Your wallet",
  "modal.account.copy": "Copy address",
  "modal.account.copied": "Copied",
  "modal.account.network": "Network",
  "modal.account.shielded": "Shielded wallet",
  "modal.account.unlock": "Unlock shielded wallet",
  "modal.account.lock": "Lock & forget",
  "modal.account.disconnect": "To disconnect, remove this site from your wallet's connected sites.",
  "phase.locked": "Locked",
  "phase.unlocking": "Unlocking",
  "phase.syncing": "Syncing",
  "phase.ready": "Ready",
  "phase.error": "Error",
} as const;

type Key = keyof typeof EN;

const ES: Record<Key, string> = {
  "nav.trade": "Operar",
  "nav.stocks": "Acciones y RWA",
  "nav.stocks.short": "Acciones",
  "nav.earn": "Ganar y bóvedas",
  "nav.earn.short": "Ganar",
  "nav.portfolio": "Portafolio",
  "header.connect": "Conectar billetera",
  "header.noWallet": "Sin billetera",
  "header.switch": "Cambiar de red",
  "header.wrongNetwork": "Red incorrecta. Los datos de mercado siguen en vivo; las transacciones están desactivadas hasta que la billetera cambie a {chain}.",
  "footer.disclaimer": "Software sin auditoría externa. Solo para redes de prueba. La contraparte es el contrato inteligente, no una empresa.",
  "footer.tagline": "Reserva del 100 %. Cero inflación. Privacidad por diseño.",
  "footer.developers": "Desarrolladores",
  "footer.docs": "Documentación",
  "footer.security": "Seguridad",
  "footer.audits": "Auditorías",
  "footer.auditsNote": "Revisión interna publicada; auditoría externa pendiente.",
  "footer.company": "Empresa",
  "footer.terms": "Términos del servicio",
  "footer.support": "Soporte",
  "footer.noEmissions": "Sin farms · sin loterías · sin emisión de tokens",
  "terms.title": "Términos del servicio",
  "terms.body1": "Los Términos del servicio se publicarán en esta página antes del lanzamiento público de StrawberrySwap.",
  "terms.body2": "Hasta entonces, esta versión es software previo al lanzamiento, solo para evaluación en redes de prueba.",
  "terms.support": "¿Preguntas? Abre un issue en GitHub:",
  "terms.back": "Volver a la aplicación",
  "settings.title": "Ajustes",
  "settings.language": "Idioma",
  "settings.fiat": "Moneda de referencia",
  "settings.fiatNote": "Los valores en moneda fiat solo usan tasas reales: USD mediante la stablecoin en USD del pool. EUR y MXN requieren una fuente de tipos de cambio, que esta versión no tiene.",
  "settings.i18nNote": "La navegación, los diálogos y los títulos siguen este idioma; las pantallas de operación están en inglés por ahora.",
  "settings.button": "Ajustes: {lang}, {fiat}",
  "fiat.peg": "a 1 {stable} = 1 USD",
  "fiat.noRate": "Sin fuente de tipo de cambio para {fiat} en esta versión",
  "fiat.noStable": "Requiere una stablecoin en USD en el pool",
  "page.trade.title": "Operar",
  "page.trade.subtitle": "Los swaps privados se liquidan en lotes a un precio uniforme. Sin comisión minorista; las pruebas se generan en este dispositivo.",
  "page.stocks.title": "Acciones y RWA",
  "page.stocks.subtitle": "Exposición sintética a precios del mundo real, totalmente colateralizada y liquidada con cotizaciones atestiguadas por pruebas de conocimiento cero.",
  "page.earn.title": "Ganar y bóvedas",
  "page.earn.subtitle": "Bóvedas de estrategia sin custodia y depósitos bancarios segregados. Nadie más que tú puede retirar tus activos.",
  "page.portfolio.title": "Portafolio",
  "page.portfolio.subtitle": "Tus saldos blindados, tu posición en la bóveda y tu historial de swaps privados.",
  "modal.connect.title": "Conecta una billetera",
  "modal.connect.subtitle": "StrawberrySwap nunca ve tus claves privadas.",
  "modal.close": "Cerrar",
  "modal.browser.name": "Billetera del navegador",
  "modal.browser.desc": "Usa la extensión de billetera de este navegador.",
  "modal.browser.detected": "Detectada",
  "modal.browser.missing": "No instalada",
  "modal.shielded.name": "Billetera blindada",
  "modal.shielded.desc": "Conéctate y desbloquea tus saldos privados con una sola firma. Las pruebas se generan en este dispositivo.",
  "modal.shielded.badge": "Nativa · ZK",
  "modal.shielded.unavailable": "No incluida en esta versión",
  "modal.hardware.name": "Firmador de hardware",
  "modal.hardware.desc": "Ledger, Trezor o GridPlus, a través de la billetera de tu navegador.",
  "modal.hardware.step1": "Conecta y desbloquea el dispositivo, y abre su aplicación de Ethereum.",
  "modal.hardware.step2": "En la billetera del navegador (MetaMask, Rabby, Frame…), añade la cuenta de hardware y selecciónala.",
  "modal.hardware.step3": "Continúa abajo: cada firma se confirma en el propio dispositivo.",
  "modal.hardware.continue": "Continuar con la billetera del navegador",
  "modal.walletconnect.name": "WalletConnect",
  "modal.walletconnect.desc": "Vincula una billetera móvil mediante la red de relés de WalletConnect, un tercero al que esta versión para IPFS no se conecta por diseño.",
  "modal.walletconnect.badge": "No incluida en esta versión",
  "modal.back": "Atrás",
  "modal.waiting": "Confirma en tu billetera…",
  "modal.switchFirst": "Cambia tu billetera a {chain} para desbloquear.",
  "modal.footer": "Las claves blindadas se derivan en este dispositivo y nunca salen de él.",
  "modal.account.title": "Tu billetera",
  "modal.account.copy": "Copiar dirección",
  "modal.account.copied": "Copiada",
  "modal.account.network": "Red",
  "modal.account.shielded": "Billetera blindada",
  "modal.account.unlock": "Desbloquear billetera blindada",
  "modal.account.lock": "Bloquear y olvidar",
  "modal.account.disconnect": "Para desconectarte, elimina este sitio de los sitios conectados de tu billetera.",
  "phase.locked": "Bloqueada",
  "phase.unlocking": "Desbloqueando",
  "phase.syncing": "Sincronizando",
  "phase.ready": "Lista",
  "phase.error": "Error",
};

const DICT: Record<Lang, Record<Key, string>> = { en: EN, es: ES };
const STORAGE_KEY = "strawberryswap.settings.v1";

interface SettingsValue {
  lang: Lang;
  fiat: Fiat;
  setLang: (l: Lang) => void;
  setFiat: (f: Fiat) => void;
  /** Translate a shell string; `{name}` placeholders are filled from `vars`. */
  t: (key: Key, vars?: Record<string, string>) => string;
  /** BCP 47 locale for number and currency formatting. */
  locale: string;
}

const SettingsContext = createContext<SettingsValue | null>(null);

function readStored(): { lang?: Lang; fiat?: Fiat } {
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return {};
    const v = JSON.parse(raw) as { lang?: unknown; fiat?: unknown };
    return {
      lang: v.lang === "en" || v.lang === "es" ? v.lang : undefined,
      fiat: FIATS.includes(v.fiat as Fiat) ? (v.fiat as Fiat) : undefined,
    };
  } catch {
    return {}; // private mode, blocked storage: defaults still work
  }
}

/** Language + reference currency, remembered per browser (localStorage; nothing leaves the device). */
export function SettingsProvider({ children }: { children: ReactNode }) {
  // Prerender and first client render use the defaults (no hydration mismatch); stored/browser preferences apply on mount.
  const [lang, setLang] = useState<Lang>("en");
  const [fiat, setFiat] = useState<Fiat>("USD");
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    const s = readStored();
    setLang(s.lang ?? (navigator.language?.toLowerCase().startsWith("es") ? "es" : "en"));
    if (s.fiat) setFiat(s.fiat);
    setLoaded(true);
  }, []);

  useEffect(() => {
    document.documentElement.lang = lang;
    if (!loaded) return;
    try {
      window.localStorage.setItem(STORAGE_KEY, JSON.stringify({ lang, fiat }));
    } catch {
      /* storage unavailable: the choice lasts for this page only */
    }
  }, [lang, fiat, loaded]);

  const t = useCallback(
    (key: Key, vars?: Record<string, string>) => {
      let s: string = DICT[lang][key] ?? EN[key];
      for (const [k, v] of Object.entries(vars ?? {})) s = s.replaceAll(`{${k}}`, v);
      return s;
    },
    [lang],
  );
  const locale = lang === "es" ? (fiat === "MXN" ? "es-MX" : "es-ES") : "en-US";
  const value = useMemo(() => ({ lang, fiat, setLang, setFiat, t, locale }), [lang, fiat, t, locale]);
  return <SettingsContext.Provider value={value}>{children}</SettingsContext.Provider>;
}

export function useSettings(): SettingsValue {
  const s = useContext(SettingsContext);
  if (!s) throw new Error("useSettings() must be used inside <SettingsProvider>");
  return s;
}

/**
 * A value in the selected reference currency, from an amount in a USD stablecoin. Only USD has a real rate here
 * (the stablecoin's 1:1 peg, stated next to the figure); EUR and MXN return no number rather than a guessed rate.
 */
export function useFiat() {
  const { fiat, locale, t } = useSettings();
  return useCallback(
    (usdStable: number | undefined, stableSymbol: string | undefined): { text?: string; note: string } => {
      if (usdStable === undefined || !stableSymbol) return { note: t("fiat.noStable") };
      if (fiat !== "USD") return { note: t("fiat.noRate", { fiat }) };
      return {
        text: new Intl.NumberFormat(locale, { style: "currency", currency: "USD", maximumFractionDigits: 2 }).format(usdStable),
        note: t("fiat.peg", { stable: stableSymbol }),
      };
    },
    [fiat, locale, t],
  );
}

/** Header control: a globe button opening a small panel with the language and currency selectors. */
export function SettingsMenu() {
  const { lang, fiat, setLang, setFiat, t } = useSettings();
  const [open, setOpen] = useState(false);
  const panelId = useId();
  const wrap = useRef<HTMLDivElement>(null);
  const button = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (wrap.current && !wrap.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setOpen(false);
        button.current?.focus();
      }
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const langLabel = LANGS.find((l) => l.value === lang)?.label ?? lang;
  return (
    <div ref={wrap} className="relative">
      <button
        ref={button}
        type="button"
        aria-expanded={open}
        aria-controls={panelId}
        aria-label={t("settings.button", { lang: langLabel, fiat })}
        onClick={() => setOpen((o) => !o)}
        className={`flex h-10 items-center gap-1.5 whitespace-nowrap rounded-full border px-3 text-sm font-semibold transition focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-fuchsia ${
          open ? "border-fuchsia bg-plum-700 text-ink shadow-glow-fuchsia" : "border-lavender/15 bg-plum-800/70 text-lavender hover:border-fuchsia/60 hover:text-ink"
        }`}
      >
        <IconGlobe className="h-5 w-5" />
        <span className="hidden uppercase xl:inline">{lang} · {fiat}</span>
      </button>
      {open ? (
        <div
          id={panelId}
          role="dialog"
          aria-label={t("settings.title")}
          className="absolute right-0 top-full z-50 mt-2 w-[min(20rem,calc(100vw-2rem))] animate-fade-in rounded-3xl border border-lavender/15 bg-plum-900/95 p-5 shadow-card backdrop-blur-xl"
        >
          <h2 className="text-base font-bold">{t("settings.title")}</h2>
          <div className="mt-4">
            <div className="eyebrow mb-2">{t("settings.language")}</div>
            <Segmented label={t("settings.language")} value={lang} onChange={setLang} options={LANGS} className="w-full [&>button]:flex-1" />
          </div>
          <div className="mt-4">
            <div className="eyebrow mb-2">{t("settings.fiat")}</div>
            <Segmented
              label={t("settings.fiat")}
              value={fiat}
              onChange={setFiat}
              options={FIATS.map((f) => ({ value: f, label: f }))}
              className="w-full [&>button]:flex-1"
            />
            <p className="mt-2 text-xs leading-relaxed text-haze">{t("settings.fiatNote")}</p>
          </div>
          <p className="mt-4 border-t border-lavender/10 pt-3 text-xs leading-relaxed text-haze">{t("settings.i18nNote")}</p>
        </div>
      ) : null}
    </div>
  );
}
