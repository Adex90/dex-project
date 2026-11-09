import type { Metadata, Viewport } from "next";
import { GeistMono } from "geist/font/mono";
import { GeistSans } from "geist/font/sans";

import "./globals.css";

export const metadata: Metadata = {
  title: { default: "Synthetic Capital", template: "%s · Synthetic Capital" },
  description:
    "A 100% reserve bank. Every peso in your vault is held in custody, and you can verify it cryptographically.",
};

export const viewport: Viewport = {
  // "Sovereign Gold" is dark only (globals.css): the browser chrome matches the black ground.
  themeColor: "#000000",
  colorScheme: "dark",
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en" className={`dark ${GeistSans.variable} ${GeistMono.variable}`}>
      <body className="min-h-dvh font-sans antialiased">{children}</body>
    </html>
  );
}
