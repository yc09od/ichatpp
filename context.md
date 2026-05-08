# AI Context — ichatpp

> 本文件供 AI 助手（如 Claude）在参与本项目开发时参考。
> 每次开启新对话时，请将本文件作为上下文提供给 AI。

## 项目快速摘要

**ichatpp** 是一个开源的 Web 实时通讯应用，类似微信。**核心特性：** 用户必须使用邀请码注册（仅管理员可生成），登录使用 email + password，已注册用户可通过账号码（10 位数字）添加好友、进行实时文字聊天、自动保存消息记录、上传自定义表情。前端采用 **Next.js（App Router）+ TypeScript + Tailwind CSS + TanStack Query + 浏览器原生 WebSocket**（不使用 Socket.io，不使用 Next Auth），后端采用 **Rust + Actix-web + actix-ws（WebSocket，Session + MessageStream，不使用 actor 模型）+ SQLx + JWT**。数据存储使用 **PostgreSQL**、缓存使用 **Redis**、对象存储使用 **MinIO/S3**。**生产部署目标平台：Coolify（开源自托管 PaaS）**，单域名同源拓扑（Caddy/Traefik 按路径分流前后端），见 ARCHITECTURE.md §7。项目目前处于 MVP 开发阶段。

## 技术约束

- **前端语言**：TypeScript（严格模式）
- **前端框架**：Next.js 14.x（使用 App Router，不用 Pages Router）
- **前端数据获取**：TanStack Query 5.x（不使用 SWR）
- **前端 WebSocket**：浏览器原生 WebSocket API（不使用 Socket.io，与后端协议保持一致）
- **前端认证**：JWT 由后端 `Set-Cookie` 写入 **httpOnly + Secure + SameSite=Lax** cookie，前端 JS **不接触 token**；fetch 必须 `credentials:'include'`，mutation 请求附带 `X-CSRF-Token` 头（取自非 httpOnly 的 csrf_token cookie）；**不使用 Next Auth**
- **后端语言**：Rust
- **后端框架**：Actix-web 4.x
- **后端 WebSocket**：actix-ws 0.3.x（Session + MessageStream + tokio task 模型；**不使用 actor 模型，不使用 tokio-tungstenite，不使用已废弃的 actix-web-actors**）
- **代码风格**：
  - 前端：遵循 Next.js 和 React 最佳实践，组件使用函数式 + Hooks
  - 后端：遵循 Rust 标准库指南，使用 cargo clippy 检查
- **包管理**：
  - 前端：npm + package-lock.json
  - 后端：Cargo + Cargo.lock
- **生产部署平台**：Coolify（自托管 PaaS）。单域名同源（前后端共享 `chat.example.com`，由 Coolify Caddy/Traefik 按 `/api/*` vs `/*` 路径分流），消除 CORS、cookie 天然 same-site。备选：手写 Nginx + docker-compose
- **测试要求**：
  - 前端：单元测试覆盖率 > 60%
  - 后端：单元测试覆盖率 > 80%
- **代码注释**：用中文或英文均可，保持统一
- **Git 规范**：
  - Commit message 格式：`[前端|后端|docs] 类型: 简短描述`
  - 例：`[后端] feat: 实现 WebSocket 消息推送` 或 `[前端] fix: 修复聊天窗口滚动问题`

## AI 工作规范

### 必须遵守

- [ ] 所有新文件必须符合现有目录结构（见 ARCHITECTURE.md 第 3 节）
- [ ] 新增依赖前必须在 ARCHITECTURE.md 中检查是否已列出，未列出的需要评估和更新
- [ ] WebSocket 消息格式必须符合 ARCHITECTURE.md 第 4.7 节定义
- [ ] 所有 API 端点必须遵循 ARCHITECTURE.md 第 6 节的 REST 设计原则
- [ ] 密码处理必须使用 bcrypt（后端），不得明文存储
- [ ] **邀请码处理必须使用 SHA-256 哈希存储，生成时使用加密强随机源；明文仅在生成响应中一次性返回**
- [ ] **所有邀请码管理端点（/api/invitations/*）必须验证管理员权限（role = "admin"）**
- [ ] **认证 token 只能存在后端写入的 httpOnly + Secure + SameSite=Lax cookie 中**；不得通过 Authorization 头、localStorage、sessionStorage、非 httpOnly cookie 等方式让前端 JS 接触 access_token / refresh_token
- [ ] **所有 mutation（POST/PUT/DELETE/PATCH）必须经过 CSRF 中间件校验**（双提交 cookie：比对 `X-CSRF-Token` 头与 csrf_token cookie）
- [ ] 数据库 schema 变更必须编写 SQL 迁移文件（在 `backend/migrations/` 中）
- [ ] 前端路由结构必须使用 Next.js App Router 的约定（`(group)` 分组、 `[id]` 动态路由）
- [ ] 所有数据库查询必须使用参数化查询（防止 SQL 注入）
- [ ] 用户注册时必须验证邀请码的有效性（未使用、未过期）
- [ ] 用户成功注册后必须将邀请码标记为 'used' 并记录使用时间和使用者 ID

### 优先原则

1. **功能正确性** > 代码优雅性
2. **类型安全** > 灵活性（TypeScript strict mode、Rust 类型系统）
3. **保持现有接口稳定**，新增而非修改
4. **遇到不确定的需求，先询问而非假设**
5. **跨端调试困难，实现时优先考虑兼容性和错误处理**

### 禁止事项

- 🚫 不得修改以下核心配置文件：`README.md`、`PRD.md`、`ARCHITECTURE.md`、`context.md`、`.github/workflows/` 
  （如需修改，先触发 scaffold-project-update 命令）
- 🚫 不得修改或删除现有 API 端点的签名（可以扩展，不能破坏）
- 🚫 不得改变 PostgreSQL 数据库 schema 而不编写迁移文件
- 🚫 不得在生产环境代码中硬编码密钥、密码、API_KEY（必须从环境变量读取）
- 🚫 不得使用 `ANY` 类型（TypeScript）、`unsafe {}` 块（Rust）除非万不得已，并添加注释说明原因
- 🚫 不得向外部暴露用户密码哈希值、刷新令牌等敏感信息
- 🚫 不得把 access_token / refresh_token 写入 localStorage、sessionStorage 或非 httpOnly cookie；不得通过 JSON 响应体下发 token；不得在前端 JS 中读取或转发 token
- 🚫 不得修改已完成的 TODO 条目（标记为 [x] 的任务）
- 🚫 不得删除 PENDING.md 文件

## 当前开发重点

**阶段：MVP 核心功能开发**

详细任务清单见 `TODO.md`，按阶段 0–7 顺序执行。整体节奏：

1. ✅ 项目架构和脚手架搭建（包含邀请码系统设计）
2. 🔲 阶段 0：项目初始化（仓库、Docker、Cargo、Next.js）— TODO [1]–[6]
3. 🔲 阶段 1：后端基础设施（配置、错误体系、迁移、CORS、CSRF）— TODO [7]–[11]
4. 🔲 阶段 2：认证与邀请码系统（JWT、cookie、邀请码 CRUD、注册登录）— TODO [12]–[20]
5. 🔲 阶段 3：用户与好友模块 — TODO [21]–[25]
6. 🔲 阶段 4：实时通讯（WebSocket）— TODO [26]–[30]
7. 🔲 阶段 5：消息记录与表情 — TODO [31]–[36]
8. 🔲 阶段 6：前端实现（认证页、聊天、好友、表情、管理后台）— TODO [37]–[46]
9. 🔲 阶段 7：测试、文档与部署 — TODO [47]–[55]

**不在当前范围内的工作：**
- 群组聊天（后续迭代）
- 消息加密（后续迭代）
- 移动端原生应用（暂时只做响应式网页设计）

## 已知问题与技术债务

### 已解决的设计决策

- ✅ 邀请码强制化 — 所有新用户必须用邀请码注册
- ✅ 管理员权限 — 仅管理员可生成/撤销邀请码
- ✅ 邀请码有效期 — 7 天，使用后失效
- ✅ 初始管理员 — 通过 seed 脚本创建

### 待实现的技术细节

- [ ] Seed 脚本实现（创建初始管理员账户）— 见 TODO [20]
- [ ] 邀请码过期自动清理任务（定时任务或 TTL）— 待补充至 TODO，可考虑依赖 invitations.expires_at + 后台扫描
- [ ] 邀请码 CSV 导出功能 — 见 TODO [15]
- [ ] 邀请码批量验证 API（前端提前检查）— 见 TODO [16]

### 已知限制

| 问题 | 影响范围 | 优先级 | 备注 |
|------|---------|--------|------|
| WebSocket 断线重连逻辑未实现 | 网络不稳定场景 | 高 | 需要在客户端加重试机制 |
| 邀请码过期清理机制未完全设计 | 数据库无用记录堆积 | 中 | 需要实现定时清理任务或 TTL |
| 离线消息暂存方案未完全设计 | 用户离线期间的消息推送 | 中 | 可用 Redis List 暂存 |
| 并发消息处理没有测试 | 高负载场景 | 中 | 需要压力测试验证 |
| 没有审计日志 | 安全性、合规性 | 低 | 后续考虑添加 |
| 管理员权限系统还很基础 | 权限控制 | 低 | 当前仅支持 user/admin，后续可扩展为 RBAC |
| Coolify 部署中 JWT 私钥需挂载持久卷 | 容器重建后 token 失效风险 | 中 | 见 ARCHITECTURE.md §7.3；首次部署需 `openssl genpkey` 一次后挂卷长期复用 |

## 工作流和沟通约定

### 开发步骤

1. **理解需求**：AI 收到任务后，先确认理解是否正确，询问任何不明确的点
2. **设计**：如果是新功能，先给出设计概要（涉及哪些模块、数据流）
3. **实现**：按照既定的技术栈和代码规范编写代码
4. **测试**：添加单元测试，确保功能正确
5. **文档**：更新相关的 .md 文档或代码注释

### 遇到问题时

- 如果代码会影响现有功能，先说明影响范围
- 如果需要新依赖，先列出候选和理由，等待确认
- 如果遇到设计上的矛盾（例如 PRD 和现实冲突），立即提出
- 不清楚的需求，直接问，不要瞎猜

## 参考文件索引

| 文件 | 用途 | 重点内容 |
|------|------|----------|
| README.md | 项目总览 | 快速开始、技术栈简介 |
| PRD.md | 产品需求 | 功能清单、用户流程、商业逻辑 |
| ARCHITECTURE.md | 技术架构 | 系统设计、数据模型、API 端点、技术决策 |
| TODO.md | 任务清单 | 待完成的开发工作 |
| PENDING.md | 待定内容 | 还在思考的想法，AI 不应该处理 |
| docs/API.md | API 详细文档（**待生成，见 TODO 阶段 7**） | 每个端点的请求/响应示例 |
| docs/DATABASE.md | 数据库设计（**待生成，见 TODO 阶段 7**） | SQL schema、索引策略 |
| docs/DEPLOYMENT.md | 部署指南（**待生成，见 TODO 阶段 7**） | 生产环境配置、Docker 构建 |

## 特殊指令

### 何时更新 context.md

当以下情况发生时，需要通知用户并触发 scaffold-project-update：
- 技术栈发生变化（例如替换某个依赖库）
- 架构发生重大调整（例如改为微服务）
- 新的约束条件（例如性能要求、安全政策）
- 当前开发重点发生变化

### 如何处理 PENDING.md

- 如果用户在 PENDING.md 中添加了内容，触发 plan-todo 命令将其转化为 TODO 条目
- **不要直接修改已完成的任务** — 如果任务做错了，新建 Issue 而不是改历史记录
- PENDING.md 是用户的思考空间，AI 应该尊重其内容但不强制执行

---

*最后更新：2026-05-07（同步 scaffold-project-update：生产部署平台定为 Coolify，ARCHITECTURE.md §7 重写为 Coolify 单域名同源方案 + Nginx 备选）*
*维护者：ichatpp 开发团队*
