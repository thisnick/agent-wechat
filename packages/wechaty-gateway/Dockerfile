FROM node:24-slim AS builder

WORKDIR /app

# Install pnpm
RUN corepack enable && corepack prepare pnpm@9.15.4 --activate

# Copy workspace root files
COPY package.json pnpm-workspace.yaml pnpm-lock.yaml turbo.json .npmrc ./

# Copy only the packages needed for the gateway
COPY packages/shared/package.json packages/shared/
COPY packages/wechaty-puppet/package.json packages/wechaty-puppet/
COPY packages/wechaty-gateway/package.json packages/wechaty-gateway/

# Install dependencies
RUN pnpm install --frozen-lockfile

# Copy source files
COPY packages/shared/ packages/shared/
COPY packages/wechaty-puppet/ packages/wechaty-puppet/
COPY packages/wechaty-gateway/ packages/wechaty-gateway/

# Build all packages (turbo handles dependency order)
RUN pnpm build --filter=@agent-wechat/wechaty-gateway...

# Deploy: creates a self-contained directory with all deps resolved
RUN pnpm --filter=@agent-wechat/wechaty-gateway deploy /deploy --prod

# ---

FROM node:24-slim

WORKDIR /app

COPY --from=builder /deploy .

EXPOSE 8788

CMD ["node", "dist/main.js"]
