// STATIC EXPORT FOR IPFS — nothing here may require a Node server at runtime.
//  * output:"export"        -> `next build` writes a fully static site to ./out (HTML/JS/CSS only)
//  * trailingSlash:true     -> /trade/ resolves to trade/index.html, which is what IPFS gateways serve
//  * images.unoptimized     -> no image-optimisation server
//  * no API routes, no server actions, no middleware, no next/font (no build- or run-time third parties)
// Config values (RPC URL, contract addresses) are NEXT_PUBLIC_* and are INLINED AT BUILD TIME, so the CID
// of a build commits to the exact addresses it talks to.
//
// GATEWAY NOTE: Next emits root-absolute asset URLs (/_next/...). Those work when the site is served from
// the ROOT of an origin: ipfs:// via a local node / IPFS Companion, a subdomain gateway
// (<cid>.ipfs.<gateway>), or an ENS/DNSLink name. They do NOT work from a path gateway
// (https://gateway/ipfs/<cid>/), because /_next then resolves against the gateway root.
/** @type {import('next').NextConfig} */
const nextConfig = {
  output: "export",
  trailingSlash: true,
  images: { unoptimized: true },
  reactStrictMode: true,
  poweredByHeader: false,
  productionBrowserSourceMaps: false,
};

export default nextConfig;
