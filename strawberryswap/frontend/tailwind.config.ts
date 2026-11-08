import type { Config } from "tailwindcss";

// "Cyber-Strawberry": a solid, matte midnight-plum canvas (no gradients); strawberry red + neon fuchsia only for the
// primary action, active/selected states, hover and focus, and data (sparklines, chart); neon mint for gains/APY,
// warning pink for slippage. No plain blacks or greys: every neutral is plum- or lavender-tinted.
// Contrast on `plum-950` (on a `plum-800` card): ink 18.1 (16.3), lavender 9.0 (8.1), haze 5.3 (4.8), berry 5.4 (4.8),
// mint 15.2 (13.7), blush 9.7 (8.8): all WCAG AA. White on the bright brand hexes is only 3.8 / 3.55:1, so button fills
// use the *-deep shades (white 4.96 / 5.73:1) and the bright hexes drive glows, accents and text.
const config: Config = {
  content: ["./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      colors: {
        plum: {
          950: "#080214", // page
          900: "#0f0520",
          850: "#14072a",
          800: "#1b0a36", // card base
          700: "#26104a",
          600: "#35165f",
          500: "#4a1f80",
          400: "#6b33ad",
        },
        berry: { DEFAULT: "#ff1a53", deep: "#e0003d", soft: "#ff5c85" },
        fuchsia: { DEFAULT: "#ff2a85", deep: "#c8006a", soft: "#ff6fae" },
        mint: { DEFAULT: "#00ff88", deep: "#00c46a" },
        blush: "#ff8fc8", // warning pink (slippage, cautions)
        ink: "#f7eefe", // primary text
        lavender: "#b9a3d9", // secondary text
        haze: "#8f78b3", // tertiary text, placeholders
      },
      fontFamily: {
        // Self-hosted from npm (@fontsource-variable/*): the export must not reach a font CDN (scripts/verify-static.mjs).
        sans: ['"Outfit Variable"', "Outfit", "ui-rounded", "system-ui", "-apple-system", "Segoe UI", "sans-serif"],
        mono: ['"JetBrains Mono Variable"', "ui-monospace", "SFMono-Regular", "Menlo", "Consolas", "monospace"],
      },
      boxShadow: {
        // matte: a 1px top highlight only, no outer shadow, so the canvas around a card stays exactly #080214
        card: "inset 0 1px 0 rgba(247, 238, 254, 0.05)",
        "glow-berry": "0 0 0 1px rgba(255, 26, 83, 0.35), 0 10px 32px -6px rgba(255, 26, 83, 0.55)",
        "glow-fuchsia": "0 0 24px -2px rgba(255, 42, 133, 0.65)",
        "glow-mint": "0 0 16px -2px rgba(0, 255, 136, 0.55)",
        inset: "inset 0 1px 0 rgba(247, 238, 254, 0.05), inset 0 0 0 1px rgba(185, 163, 217, 0.08)",
      },
      keyframes: {
        // primary action ready: only the glow breathes (opacity stays 1, so the label keeps its contrast)
        "cta-glow": {
          "0%, 100%": { boxShadow: "0 0 0 1px rgba(255, 26, 83, 0.35), 0 10px 32px -6px rgba(255, 26, 83, 0.55)" },
          "50%": { boxShadow: "0 0 0 1px rgba(255, 42, 133, 0.6), 0 12px 44px -2px rgba(255, 42, 133, 0.85)" },
        },
        stripes: { from: { backgroundPosition: "0 0" }, to: { backgroundPosition: "28px 0" } },
        "sheet-up": { from: { transform: "translateY(24px)", opacity: "0" }, to: { transform: "translateY(0)", opacity: "1" } },
        "fade-in": { from: { opacity: "0" }, to: { opacity: "1" } },
        float: { "0%, 100%": { transform: "translateY(0)" }, "50%": { transform: "translateY(-6px)" } },
        indeterminate: { from: { transform: "translateX(-100%)" }, to: { transform: "translateX(250%)" } },
      },
      animation: {
        "cta-glow": "cta-glow 2.6s ease-in-out infinite",
        stripes: "stripes 0.9s linear infinite",
        "fade-in": "fade-in 320ms ease-out both",
        "sheet-up": "sheet-up 280ms cubic-bezier(0.2, 0.8, 0.2, 1) both",
        float: "float 4.5s ease-in-out infinite",
        indeterminate: "indeterminate 1.6s ease-in-out infinite",
      },
    },
  },
  plugins: [],
};
export default config;
