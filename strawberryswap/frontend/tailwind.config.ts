import type { Config } from "tailwindcss";

// Sober midnight palette. Contrast (on `bg`): text 13.6:1, muted 6.2:1, accent 8.1:1 — all clear WCAG AA.
const config: Config = {
  content: ["./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      colors: {
        bg: "#0b0e14", // page
        surface: "#0f131b", // panels / inputs
        rule: "#232a36", // hairlines
        text: "#d9dde5",
        muted: "#8d95a6",
        accent: "#8fb4ff", // links, focus, primary action
        pos: "#86b98f", // positive / confirmed
        neg: "#d98585", // errors
        warn: "#d1b06b", // cautions
      },
      fontFamily: {
        // System stacks only: the site must not depend on a font CDN. Serifs resolve to Latin/Computer
        // Modern when installed, otherwise to the platform's book serif.
        serif: ['"Latin Modern Roman"', '"CMU Serif"', '"Computer Modern Serif"', '"Iowan Old Style"', '"Palatino Linotype"', "Palatino", "Georgia", "serif"],
        mono: ['"Latin Modern Mono"', "ui-monospace", "SFMono-Regular", "Menlo", "Consolas", "monospace"],
      },
    },
  },
  plugins: [],
};
export default config;
