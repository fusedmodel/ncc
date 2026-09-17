# syntax=docker/dockerfile:1

# ---------- 前端构建 ----------
FROM node:20-alpine AS web
WORKDIR /src
COPY ncc-platform/package.json ncc-platform/package-lock.json ./
COPY ncc-platform/web/package.json web/package.json
COPY opensource-project/packages/ncc-cli/package.json packages/ncc-cli/package.json
RUN npm install -w web --ignore-scripts
COPY ncc-platform/web/ web/
RUN npm run build -w web

# ---------- 服务端构建 ----------
FROM golang:1.24-alpine AS server
WORKDIR /src
COPY ncc-platform/server/go.mod ncc-platform/server/go.sum ./
RUN go mod download
COPY ncc-platform/server/ ./
RUN CGO_ENABLED=0 go build -trimpath -o /out/ncc-server ./cmd/server

# ---------- 运行时 ----------
FROM alpine:3.20
RUN apk add --no-cache ca-certificates tzdata
WORKDIR /app
COPY --from=server /out/ncc-server /app/ncc-server
COPY --from=web /src/web/dist /app/web
ENV NCC_WEB_DIST=/app/web \
    NCC_DATA_DIR=/data \
    NCC_PORT=8181
VOLUME ["/data"]
EXPOSE 8181
ENTRYPOINT ["/app/ncc-server"]
