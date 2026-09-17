# NCC Registry · Standalone / 内部 Registry

自托管（Standalone）部署：一条命令拉起完整 NCC Registry，用于**内部 / 私有环境**（企业内网、私有云）。支持可配置的存储后端：本地磁盘或 **S3 兼容对象存储（Ceph RGW / MinIO / AWS S3）**。

## 快速开始

```bash
cd standalone
cp .env.example .env          # 按需修改
docker compose up -d --build
```

启动后：

- 产品 / API：http://localhost:8181
- MinIO 控制台：http://localhost:9001（`nccadmin` / `nccadmin123`）

## 存储驱动

通过 `NCC_STORAGE_DRIVER` 选择：

| 驱动 | 值 | 说明 |
|---|---|---|
| 本地磁盘 | `local`（默认） | 字节写入 `NCC_DATA_DIR/uploads`，经 `/uploads` 公开 |
| S3 兼容对象存储 | `s3` | 走 S3 API（SigV4），兼容 **Ceph RGW** / MinIO / AWS S3 |

S3 相关配置（`NCC_STORAGE_DRIVER=s3` 时必填）：

| 变量 | 说明 |
|---|---|
| `NCC_S3_ENDPOINT` | 对象存储地址，如 `http://ceph-rgw:7480` |
| `NCC_S3_REGION` | 区域，默认 `us-east-1` |
| `NCC_S3_ACCESS_KEY` / `NCC_S3_SECRET_KEY` | 访问凭据 |
| `NCC_S3_BUCKET` | 桶名，默认 `ncc` |
| `NCC_S3_PUBLIC_BASE` | 公开访问基址（公开条目下载 URL）；留空则 `endpoint/bucket` |

## 对接 Ceph

Ceph 的 **RADOS Gateway（RGW）** 提供 S3 兼容接口，只需把 `NCC_S3_ENDPOINT` 指向 RGW 地址并配置 `NCC_S3_ACCESS_KEY/SECRET_KEY/BUCKET` 即可，代码零改动。

> 说明：`docker-compose.yml` 用 MinIO 作为开箱即用的 S3 兼容依赖（轻量、易验证）；生产可替换为 Ceph RGW，接口完全一致。

## 内置依赖说明

- `ncc`：NCC Registry 服务（API + 计费 + 前端 SPA，单二进制）。
- `minio`：S3 兼容对象存储（可作为 Ceph RGW 的替代）。
- `minio-init`：创建桶 `ncc` 并对公开条目开放匿名下载。

## 单机本地磁盘模式（不依赖对象存储）

```bash
NCC_STORAGE_DRIVER=local docker compose up -d --build
# 或直接运行二进制（相对本目录）：
NCC_WEB_DIST=../../ncc-platform/web/dist NCC_STORAGE_DRIVER=local ../../ncc-platform/server/dist/ncc-server
```
