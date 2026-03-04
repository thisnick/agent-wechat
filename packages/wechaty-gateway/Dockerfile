FROM node:22-slim AS builder

WORKDIR /app

# Install pnpm
RUN corepack enable && corepack prepare pnpm@9.15.4 --activate

# Copy workspace root files
COPY package.json pnpm-workspace.yaml pnpm-lock.yaml turbo.json ./

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

# ---

FROM node:22-slim

WORKDIR /app

RUN corepack enable && corepack prepare pnpm@9.15.4 --activate

COPY --from=builder /app/package.json /app/pnpm-workspace.yaml /app/pnpm-lock.yaml ./
COPY --from=builder /app/packages/shared/package.json packages/shared/
COPY --from=builder /app/packages/shared/dist/ packages/shared/dist/
COPY --from=builder /app/packages/wechaty-puppet/package.json packages/wechaty-puppet/
COPY --from=builder /app/packages/wechaty-puppet/dist/ packages/wechaty-puppet/dist/
COPY --from=builder /app/packages/wechaty-gateway/package.json packages/wechaty-gateway/
COPY --from=builder /app/packages/wechaty-gateway/dist/ packages/wechaty-gateway/dist/

RUN pnpm install --frozen-lockfile --prod

EXPOSE 8788

CMD ["node", "packages/wechaty-gateway/dist/main.js"]
