# Build context is the `web/` npm workspace root so the workspace lockfile
# and sibling manifests are visible; see docker-compose.review.yml.
FROM node:24-alpine AS build
WORKDIR /web
# Every workspace manifest is required for `npm ci` to validate the lockfile,
# even though only this app's dependency subtree is installed.
COPY package.json package-lock.json tsconfig.base.json ./
COPY apps/app-ui/package.json apps/app-ui/
COPY apps/review-ui/package.json apps/review-ui/
COPY packages/code-review/package.json packages/code-review/
COPY packages/content-rendering/package.json packages/content-rendering/
COPY packages/protocol/package.json packages/protocol/
COPY packages/transcript/package.json packages/transcript/
COPY packages/ui-foundation/package.json packages/ui-foundation/
RUN npm ci --workspace apps/review-ui --no-audit --no-fund
COPY apps/review-ui/tsconfig.json apps/review-ui/index.html apps/review-ui/
COPY apps/review-ui/src apps/review-ui/src
# The shared packages are consumed as TypeScript source through their package
# exports, so their sources (and the tsconfigs Vite reads for class-field
# semantics) are part of this build context too: the dashboard itself and
# the design system it is drawn with.
COPY packages/code-review/tsconfig.json packages/code-review/
COPY packages/code-review/src packages/code-review/src
COPY packages/ui-foundation/tsconfig.json packages/ui-foundation/
COPY packages/ui-foundation/src packages/ui-foundation/src
RUN npm run build --workspace apps/review-ui

FROM nginxinc/nginx-unprivileged:1.29.4-alpine
COPY apps/review-ui/nginx.conf /etc/nginx/conf.d/default.conf
COPY --from=build /web/apps/review-ui/dist /usr/share/nginx/html
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
  CMD wget -q -O /dev/null http://127.0.0.1:8080/ || exit 1
USER nginx
