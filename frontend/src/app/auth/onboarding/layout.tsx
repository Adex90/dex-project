import type { Metadata } from "next";

export const metadata: Metadata = {
  title: "Verify your identity",
  description: "Digital onboarding: personal data, INE and a liveness selfie to verify your identity.",
};

export default function OnboardingLayout({ children }: { children: React.ReactNode }) {
  return children;
}
