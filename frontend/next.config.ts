import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  reactStrictMode: true,
  poweredByHeader: false,
  async headers() {
    return [
      {
        source: "/:path*",
        headers: [
          { key: "X-Frame-Options", value: "DENY" },
          { key: "X-Content-Type-Options", value: "nosniff" },
          { key: "Referrer-Policy", value: "strict-origin-when-cross-origin" },
          // camera=(self): the onboarding wizard (/auth/onboarding) photographs the
          // INE and takes the liveness selfie. It must hold for every page, not
          // only that route: a document keeps the policy it was loaded with, so
          // after a client-side navigation (e.g. dashboard -> onboarding) a
          // route-scoped header would never apply. Third-party frames still get
          // no camera, and the browser still asks the user for permission.
          { key: "Permissions-Policy", value: "camera=(self), microphone=(), geolocation=()" },
        ],
      },
    ];
  },
};

export default nextConfig;
