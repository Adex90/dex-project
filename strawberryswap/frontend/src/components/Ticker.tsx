const ITEMS = [
  "0% RETAIL FEE",
  "100% RESERVE",
  "NO MINT FUNCTION",
  "NO ADMIN KEY",
  "MEV PAYS THE GAS",
  "DEMAND DEPOSITS CANNOT BE LENT",
  "THE MANAGER HAS NO WITHDRAW",
  "STATIC SITE · PINNED ON IPFS",
];

/** Marquee strip: part wire-service ticker, part photocopied flyer. Pure CSS, no JS. */
export function Ticker() {
  const row = [...ITEMS, ...ITEMS];
  return (
    <div className="overflow-hidden border-y-2 border-paper bg-acid py-1 text-ink" aria-hidden="true">
      <div className="ticker-track flex w-max gap-10 whitespace-nowrap font-slab text-lg uppercase tracking-widest">
        {row.map((t, i) => (
          <span key={i}>★ {t}</span>
        ))}
      </div>
    </div>
  );
}
