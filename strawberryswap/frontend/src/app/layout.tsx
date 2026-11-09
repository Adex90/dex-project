import type { Metadata, Viewport } from "next";
// Fonts are bundled from npm into /_next/static/media: the static export never contacts a font CDN.
import "@fontsource-variable/outfit";
import "@fontsource-variable/jetbrains-mono";
import "@/styles/globals.css";
import { SettingsProvider } from "@/components/settings/Settings";

export const metadata: Metadata = {
  title: "StrawberrySwap — zero-fee private dark pool",
  description: "Private swaps on a zero-fee dark pool. 100% reserve. Proofs are generated on your device. No admin keys.",
  robots: { index: true, follow: true },
  applicationName: "StrawberrySwap",
  // The mascot as favicon and app icon; files in public/ (generated from public/icon.svg). Root-absolute like every
  // other asset of the export (see next.config.mjs on IPFS gateways).
  icons: {
    icon: [
      { url: "/favicon.ico", sizes: "32x32" }, // 16, 32 and 48 px inside; SVG-capable browsers pick the vector below
      { url: "/icon.svg", type: "image/svg+xml" },
    ],
    apple: [{ url: "/apple-touch-icon.png", sizes: "180x180" }],
  },
  manifest: "/manifest.json",
  appleWebApp: { capable: true, title: "StrawberrySwap", statusBarStyle: "black" },
};

export const viewport: Viewport = {
  themeColor: "#080214",
  colorScheme: "dark",
  width: "device-width",
  initialScale: 1,
  viewportFit: "cover", // lets the bottom navigation pad for the home indicator (env(safe-area-inset-bottom))
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en" className="dark">
      <body>
        <SettingsProvider>{children}</SettingsProvider>
      </body>
    </html>
  );
}
