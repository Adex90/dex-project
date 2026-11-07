import type { Config } from "tailwindcss";

// Palette: bank-terminal blacks + risograph zine accents. All contrast pairs below clear WCAG AA on `ink`.
const config: Config = {
  content: ["./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      colors: {
        ink: "#0a0a0b", // page
        slab: "#141416", // panels
        rule: "#2c2d33", // hairlines
        paper: "#ece6d6", // zine paper / primary text
        ash: "#9a9aa3", // secondary text
        berry: "#ff2d55", // strawberry red: danger / brand
        acid: "#d4ff3a", // acid yellow: positive / action
        ice: "#7df9ff", // data highlight
      },
      fontFamily: {
        // System stacks only: an "indestructible" site must not depend on a font CDN.
        mono: ["ui-monospace", "SFMono-Regular", "Menlo", "Consolas", "monospace"],
        slab: ["Impact", "Haettenschweiler", "Arial Narrow Bold", "Arial Black", "sans-serif"],
        sans: ["system-ui", "-apple-system", "Segoe UI", "Helvetica Neue", "Arial", "sans-serif"],
      },
      boxShadow: {
        punk: "6px 6px 0 0 #ff2d55",
        acid: "6px 6px 0 0 #d4ff3a",
        paper: "6px 6px 0 0 #ece6d6",
      },
    },
  },
  plugins: [],
};
export default config;
