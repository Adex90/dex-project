# Synthetic Capital frontend (Next.js 15) — production image.
#
#   docker build -t synthetic-capital-frontend \
#     --build-arg NEXT_PUBLIC_API_URL=https://bank.example.mx \
#     --build-arg NEXT_PUBLIC_COMPLIANCE_EMAIL=compliance@bank.example.mx \
#     --build-arg REVISION="$(git rev-parse --short HEAD)" \
#     frontend/
#
# NEXT_PUBLIC_* values are inlined into the browser bundle at BUILD time: one image
# per public origin. Behind the nginx edge (same-origin routing of /api/ and /salud)
# NEXT_PUBLIC_API_URL is the public origin itself, without a path.
#
# Run it with a read-only root filesystem; the only writable path Next.js may use is
# /app/.next/cache. A plain `--tmpfs` is root-owned (0755), so give it to uid 65532
# (a named volume works too: Docker seeds it with the image's 65532-owned directory):
#   docker run --read-only --tmpfs /app/.next/cache:uid=65532,gid=65532,mode=0700,size=64m \
#     --cap-drop ALL --security-opt no-new-privileges --user 65532 -p 3000:3000 \
#     synthetic-capital-frontend
#
# Base images are pinned by digest in the FROM lines, where Dependabot keeps them current
# (it cannot see an image named through an ARG). A digest is a content address: behind a
# pull-through mirror (dockerd "registry-mirrors") the bytes are the same. Build and
# runtime are both Debian 13 (trixie).
#
# Behind a TLS-intercepting proxy, pass its CA as a BuildKit secret (never baked in):
#   --secret id=extra_ca,src=/path/to/corporate-ca.pem
# plus the usual HTTPS_PROXY/HTTP_PROXY/NO_PROXY build args (predefined by BuildKit and
# kept out of the image history).

FROM docker.io/library/node:26.12.0-trixie-slim@sha256:e556a144463cc8dc53afe8de6d2fa0db5c132fd39ffa232c17641bd40df2bf11 AS node

# ---------------------------------------------------------------------------------------
# deps: exact dependency tree from package-lock.json (dev dependencies included: the
# build needs TypeScript, Tailwind and ESLint; none of them reach the runtime image).
# ---------------------------------------------------------------------------------------
FROM node AS deps
WORKDIR /app
ENV NEXT_TELEMETRY_DISABLED=1 \
    npm_config_audit=false \
    npm_config_fund=false \
    npm_config_update_notifier=false
COPY package.json package-lock.json ./
# --ignore-scripts: no dependency lifecycle script runs at install time (the only one in
# the tree, unrs-resolver's, merely re-checks a native binding npm already installed).
RUN --mount=type=secret,id=extra_ca,required=false \
    --mount=type=cache,target=/root/.npm,sharing=locked \
    if [ -s /run/secrets/extra_ca ]; then export NODE_EXTRA_CA_CERTS=/run/secrets/extra_ca; fi; \
    npm ci --ignore-scripts

# ---------------------------------------------------------------------------------------
# build: `next build` with output: "standalone" (toggled by NEXT_OUTPUT in next.config.ts).
# ---------------------------------------------------------------------------------------
FROM node AS build
WORKDIR /app
ARG NEXT_PUBLIC_API_URL
ARG NEXT_PUBLIC_COMPLIANCE_EMAIL
ENV NEXT_TELEMETRY_DISABLED=1 \
    NEXT_OUTPUT=standalone \
    NODE_ENV=production
COPY --from=deps /app/node_modules ./node_modules
COPY . .
# The browser bundle builds absolute API URLs from NEXT_PUBLIC_API_URL, so an empty,
# relative or space-separated value (compose builds it as https://${PUBLIC_HOST}) would
# only fail later, in the customer's browser: refuse it here.
# sharp (an optional dependency of next, ~20 MB with libvips) is removed before the
# build so output tracing leaves it out: the app has no next/image and no public/ dir,
# and the nginx edge answers 404 for /_next/image.
RUN case "${NEXT_PUBLIC_API_URL:-}" in \
      *[[:space:]]*) echo "error: build arg NEXT_PUBLIC_API_URL contains whitespace (PUBLIC_HOST must be ONE host name)" >&2; exit 1 ;; \
      http://?*|https://?*) ;; \
      *) echo "error: build arg NEXT_PUBLIC_API_URL must be an absolute http(s) URL (the public origin behind the edge)" >&2; exit 1 ;; \
    esac; \
    if [ -z "${NEXT_PUBLIC_COMPLIANCE_EMAIL:-}" ]; then \
      echo "warning: NEXT_PUBLIC_COMPLIANCE_EMAIL not set; the bundle keeps the placeholder address" >&2; \
      unset NEXT_PUBLIC_COMPLIANCE_EMAIL; \
    fi; \
    rm -rf node_modules/sharp node_modules/@img; \
    npm run build; \
    mkdir -p /out /cache; \
    cp -a .next/standalone/. /out/; \
    cp -a .next/static /out/.next/static; \
    if [ -d public ]; then cp -a public /out/public; fi

# ---------------------------------------------------------------------------------------
# runtime: distroless Node.js 22 (no shell, no package manager), uid/gid 65532.
# ---------------------------------------------------------------------------------------
FROM gcr.io/distroless/nodejs22-debian13:nonroot@sha256:10747a1323be68f206aed893599d43e6697ad67257e7c42703553f99fbce3d33 AS runtime
# The base image's default user is 65532: create /app and its files as root, so the
# service user can neither modify nor replace application code even on a writable rootfs.
USER 0:0
ARG REVISION=unknown
LABEL org.opencontainers.image.title="synthetic-capital-frontend" \
      org.opencontainers.image.description="Synthetic Capital web frontend (Next.js standalone server)" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.base.name="gcr.io/distroless/nodejs22-debian13:nonroot"
WORKDIR /app
ENV NODE_ENV=production \
    NEXT_TELEMETRY_DISABLED=1 \
    HOSTNAME=0.0.0.0 \
    PORT=3000
COPY --from=build /out/ /app/
# The one path Next.js may write to, pre-created for the service user. VOLUME makes
# runtimes that seed volumes from the image keep that ownership: an ECS Fargate bind
# mount at this path, or Docker's anonymous/named volume when no tmpfs is given (so
# --read-only works without extra flags). An explicit tmpfs mount takes precedence.
COPY --from=build --chown=65532:65532 /cache /app/.next/cache
VOLUME ["/app/.next/cache"]
USER 65532:65532
EXPOSE 3000
# No shell or curl in distroless: probe with Node's built-in fetch.
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
  CMD ["/nodejs/bin/node", "-e", "fetch('http://127.0.0.1:'+(process.env.PORT||3000)+'/',{signal:AbortSignal.timeout(4000)}).then(r=>process.exit(r.ok?0:1),()=>process.exit(1))"]
# The distroless entrypoint is /nodejs/bin/node.
CMD ["server.js"]
