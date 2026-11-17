import type { Metadata } from "next";

export const metadata: Metadata = {
  title: "Verify your identity",
  description:
    "Identity verification in four steps: your identity, your INE, a liveness check with your declarations, and the agreements.",
};

export default function VerifyLayout({ children }: { children: React.ReactNode }) {
  return children;
}
