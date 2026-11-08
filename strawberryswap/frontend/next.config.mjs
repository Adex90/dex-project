// STATIC EXPORT FOR IPFS — nothing here may require a Node server at runtime.
//  * output:"export"        -> `next build` writes a fully static site to ./out (HTML/JS/CSS/WASM only)
//  * trailingSlash:true     -> /trade/ resolves to trade/index.html, which is what IPFS gateways serve
//  * images.unoptimized     -> no image-optimisation server
//  * no API routes, no server actions, no middleware, no next/font (no build- or run-time third parties)
// Config values (RPC URL, contract addresses) are NEXT_PUBLIC_* and are INLINED AT BUILD TIME, so the CID
// of a build commits to the exact addresses it talks to. The proving artifacts are pinned the same way
// (scripts/prepare-zk.mjs -> src/zk/manifest.generated.json).
//
// IN-BROWSER PROVING: snarkjs and the prover worker are bundled for the browser. The worker is emitted as its own
// static chunk by `new Worker(new URL("./prover.worker.ts", import.meta.url))`; Node built-ins that snarkjs's
// dependencies mention but never use in the browser are stubbed out below.
//
// GATEWAY NOTE: Next emits root-absolute asset URLs (/_next/..., /zk/...). Those work when the site is served from
// the ROOT of an origin: ipfs:// via a local node / IPFS Companion, a subdomain gateway
// (<cid>.ipfs.<gateway>), or an ENS/DNSLink name. They do NOT work from a path gateway
// (https://gateway/ipfs/<cid>/), because /_next then resolves against the gateway root.
import { fileURLToPath } from "node:url";

// snarkjs' `exports` map lists the Node entry before the browser entry and webpack picks the former, dragging in
// Node-only code (web-worker's node variant). Point the client bundle at the browser ESM build explicitly.
const snarkjsBrowser = fileURLToPath(new URL("./node_modules/snarkjs/build/browser.esm.js", import.meta.url));

/** @type {import('next').NextConfig} */
const nextConfig = {
  output: "export",
  trailingSlash: true,
  images: { unoptimized: true },
  reactStrictMode: true,
  poweredByHeader: false,
  productionBrowserSourceMaps: false,
  webpack(config, { isServer }) {
    // `web-worker` (an ffjavascript dependency) has a Node variant with a dynamic require. Only the SERVER pass
    // (prerendering) resolves it; that code never runs there, and the client pass uses the browser variant
    // (scripts/verify-static.mjs checks the client output for Node-only code). Silence exactly that warning.
    config.ignoreWarnings = [...(config.ignoreWarnings ?? []), { module: /node_modules[\\/]web-worker/, message: /Critical dependency/ }];
    // every compilation (the server pass prerenders pages that import the worker client too)
    config.resolve.alias = { ...config.resolve.alias, snarkjs: snarkjsBrowser };
    if (!isServer) {
      config.resolve.fallback = { ...config.resolve.fallback, fs: false, path: false, os: false, crypto: false, readline: false, constants: false, stream: false, util: false, worker_threads: false, child_process: false, tty: false, net: false, tls: false, http: false, https: false, zlib: false, url: false, assert: false };
    }
    return config;
  },
};

export default nextConfig;
