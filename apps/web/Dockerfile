# Container image of the web app, for sp00ky's paid plans. The free
# (Cloudflare) plan does not use it: it builds the SPA with `deploy.static`
# and publishes it as static assets. Build context: the repo root.
FROM node:22-alpine AS build
RUN corepack enable
WORKDIR /app
COPY package.json pnpm-workspace.yaml pnpm-lock.yaml ./
COPY apps/web/package.json apps/web/
COPY apps/api/package.json apps/api/
COPY scripts/package.json scripts/
RUN pnpm install --frozen-lockfile --filter @studycall/web...
COPY shared shared
COPY apps/web apps/web
RUN pnpm --filter @studycall/web build

FROM nginx:1.29-alpine
COPY apps/web/nginx.conf /etc/nginx/conf.d/default.conf
COPY --from=build /app/apps/web/dist /usr/share/nginx/html
EXPOSE 80
