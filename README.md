# opensource-project（开源组件容器）

定位：存放**可对外开源/单独分发**的独立组件；与 ncc.ai 的私有核心、用户功能及内部规划文档分离。

> 仓库根为 `ncc-ai/`，含两个顶层目录：`ncc-platform/`（私有平台工程：server / web / prd / data）与 `opensource-project/`（本目录）。

## 现有组件（已归档）

| 目录 | 内容 |
|---|---|
| `cli/` | Rust CLI（bin: ncc）——最典型的可开源客户端 |
| `packages/ncc-cli/` | npm 包装（@ncc/cli，bin:ncc；自动定位/下载 Rust 二进制） |
| `standalone/` | Dockerfile + docker-compose（自托管部署；构建上下文指向仓库根，读取 `ncc-platform/server` + `ncc-platform/web`） |
| `scripts/` | run.sh · smoke.sh · demo-skill.sh · build-release.sh |
| `release/` | 预编译二进制 + checksums.txt（build-release 产物） |

## 放（后续新增的开源部分）
- 可独立开源的 CLI / SDK / 库 / 工具 / harness / 打包与分发类组件。
- 每个子项自带：`LICENSE`、`README`、独立构建/版本化，**不依赖 `ncc-platform/server`、`ncc-platform/web` 的私有实现**。

## 不放（保持私有 / 内部）
- **核心服务**：`ncc-platform/server/`（含 `/api/registry` 等服务端逻辑）。
- **Web 产品与用户功能**：`ncc-platform/web/`（Landing / Auth / Account / Share / Namespace 及其 i18n / SEO / 样式）。
- **内部规划**：`ncc-platform/prd/`（roadmap、竞争与收敛、GtM、各产品设计/PRD）——开源内容里不夹带内部路线。
- 用户数据、密钥、内网拓扑等敏感物。

## 规则
- 新开源组件一律**新建在本目录下的独立子目录**（独立 LICENSE / README / 版本）。
- 需要调用核心能力时，只走**公开协议 / API**（如 HTTP `/api/…`），不 import `ncc-platform/server`、`ncc-platform/web` 的私有代码。
- 本 README 为内部约定说明（对外发布前可移除）。
