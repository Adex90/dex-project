import type { Metadata, Viewport } from "next";
// Fonts are bundled from npm into /_next/static/media: the static export never contacts a font CDN.
import "@fontsource-variable/outfit";
import "@fontsource-variable/jetbrains-mono";
import "@/styles/globals.css";

export const metadata: Metadata = {
  title: "StrawberrySwap — zero-fee private dark pool",
  description: "Private swaps on a zero-fee dark pool. 100% reserve. Proofs are generated on your device. No admin keys.",
  robots: { index: true, follow: true },
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
      <body>{children}</body>
    </html>
  );
}
