FROM node:22.22.0-bookworm-slim

WORKDIR /app

COPY package.json package-lock.json ./
RUN npm ci --omit=dev --ignore-scripts && npm cache clean --force

COPY src ./src

ENV NODE_ENV=production \
    MCP_WORK_ROOT=/data/work \
    MCP_COWORK_ROOT=/data/cowork

USER node

ENTRYPOINT ["node", "src/server.js"]
