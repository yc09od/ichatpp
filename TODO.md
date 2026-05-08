# TODO — ichatpp

> 本文件是项目的详细任务清单，每个任务包含对 AI 的具体指令。
> 完成后将 `[ ]` 标记为 `[x]`。任务编号格式：`-- [N]`，子任务为 `-- [N.x]`。
> **执行原则：** 严格按照阶段顺序，前置阶段未完成时不要跳到后续阶段。

---

## 阶段 0：项目初始化

-- [1] [x] **初始化仓库与目录结构**
  - AI 指令：根据 ARCHITECTURE.md 第 3 节生成完整目录骨架（`frontend/`, `backend/`, `docs/`, `.github/workflows/`），添加 `.gitignore`（覆盖 `node_modules`, `target`, `.env`, `.next`, `dist`），添加 MIT `LICENSE`
  - 验收标准：`tree -L 3` 输出与 ARCHITECTURE.md 第 3 节一致

-- [2] [x] **生成 docker-compose.yml（开发环境）**
  - AI 指令：在仓库根目录创建 `docker-compose.yml`，包含 services：`postgres:14`、`redis:6`、`minio/minio:latest`，分别暴露 5432/6379/9000+9001 端口；卷挂载持久化数据；`.env.example` 提供默认变量
  - 验收标准：`docker compose up -d` 能在本机起来，`docker compose ps` 三个服务状态为 healthy/running

-- [3] [x] **后端 Cargo 工程初始化**
  - AI 指令：在 `backend/` 下 `cargo init`，写入 `Cargo.toml`，按 ARCHITECTURE.md 第 2 节"后端"表格列出的依赖加入：actix-web 4.x, actix-ws 0.3.x, tokio 1.x, serde, serde_json, sqlx (postgres, runtime-tokio-rustls, uuid, chrono, macros), redis, jsonwebtoken, bcrypt, uuid (v4), env_logger, dotenvy, sha2, rand
  - 验收标准：`cargo build` 通过，`cargo clippy` 无 error

-- [4] [x] **前端 Next.js 工程初始化**
  - AI 指令：在 `frontend/` 下用 `create-next-app@14` 生成项目（TypeScript + App Router + Tailwind + ESLint，不要 src/ 目录），追加依赖：@tanstack/react-query 5.x, zod；不要安装 socket.io-client、不要安装 next-auth
  - 验收标准：`npm run dev` 启动并访问 http://localhost:3000 显示默认页面，`npm run build` 通过

-- [5] [x] **前端目录结构与全局 Provider**
  - AI 指令：按 ARCHITECTURE.md 第 3 节创建 `app/(auth)/`、`app/(app)/`、`components/`、`lib/`、`styles/` 目录；在 `app/layout.tsx` 中包裹 TanStack Query 的 `QueryClientProvider`；创建 `lib/api.ts` 封装基础 fetch（必须 `credentials: 'include'`，自动带 `X-CSRF-Token` 头）和 `lib/websocket.ts` 占位
  - 验收标准：根布局加载 QueryProvider 不报错；`lib/api.ts` 默认携带 cookie 和 CSRF 头

-- [6] [x] **GitHub Actions CI 工作流**
  - AI 指令：编写 `.github/workflows/frontend-ci.yml`（lint + typecheck + build + test）和 `.github/workflows/backend-ci.yml`（fmt + clippy + test），触发条件：push 到任何分支 + PR 到 main
  - 验收标准：本地 `act` 模拟通过，或推送到 GitHub 后 workflow 跑通

---

## 阶段 1：后端基础设施

-- [7] [x] **配置管理与启动入口**
  - AI 指令：实现 `backend/src/config.rs`，从环境变量加载：`DATABASE_URL`, `REDIS_URL`, `JWT_SECRET`(RS256 公私钥路径), `S3_ENDPOINT`, `S3_ACCESS_KEY`, `S3_SECRET_KEY`, `BIND_ADDR`；`main.rs` 初始化 logger、连接池（PgPool、Redis）、启动 Actix HttpServer 监听 0.0.0.0:8080
  - 验收标准：`cargo run` 后访问 `GET /api/health` 返回 `{"status":"ok"}`

-- [8] [x] **错误体系与统一响应封装**
  - AI 指令：在 `backend/src/errors/` 实现 `AppError` 枚举（含 NotFound、Unauthorized、Forbidden、BadRequest、Conflict、Internal），实现 `ResponseError`，错误体使用 ARCHITECTURE.md 第 6 节定义的 JSON 结构；提供成功响应的辅助 `ApiResponse<T>` 包装为 `{ data, meta: { timestamp } }`
  - 验收标准：手写一个返回 NotFound 的端点，响应符合错误格式

-- [9] [x] **数据库迁移：建表脚本**
  - AI 指令：在 `backend/migrations/` 下用 sqlx-cli 创建迁移文件，**严格按 ARCHITECTURE.md 第 5 节的 SQL** 顺序创建 users、user_roles、friends、friend_requests、messages、emojis、message_emojis、invitations 表及所有索引（包含 `uq_friend_requests_pending` 部分唯一索引）
  - 验收标准：`sqlx migrate run` 全部通过，`\d` 检查表结构匹配

-- [10] [x] **CORS、限流与日志中间件**
  - AI 指令：实现：(1) CORS 中间件，`Access-Control-Allow-Credentials: true`，origin 从配置读取，禁止 `*`；(2) 基于 IP 的全局限流（actix-governor 或自实现令牌桶）；(3) 请求日志中间件输出 method/path/status/耗时
  - 验收标准：跨域请求带 cookie 能正确通过；连续 11 次 POST `/api/auth/login` 第 11 次返回 429

-- [11] [x] **CSRF 中间件（双提交 cookie）**
  - AI 指令：实现 CSRF 中间件：对所有 mutation（POST/PUT/DELETE/PATCH，除 `/api/auth/login` 与 `/api/auth/register`）校验 `X-CSRF-Token` 头与 `csrf_token` cookie 是否一致；登录成功时下发 csrf_token cookie（非 httpOnly, Secure, SameSite=Lax）
  - 验收标准：登录后调用受保护 mutation 不带 X-CSRF-Token 返回 403；带正确 token 通过

---

## 阶段 2：认证与邀请码系统

-- [12] [x] **JWT 工具与 cookie 写入辅助**
  - AI 指令：实现 `backend/src/auth/jwt.rs`，提供 `sign_access_token(user_id, role)` / `sign_refresh_token(user_id)` / `verify_token(token)`；使用 RS256 非对称加密；提供 `set_auth_cookies(response, access, refresh, csrf)` 辅助函数，按 ARCHITECTURE.md 第 4.1 节定义设置 httpOnly + Secure + SameSite=Lax，refresh 限定 Path=/api/auth/refresh
  - 验收标准：单元测试覆盖签发与验证（包含过期、错误签名场景）

-- [13] [x] **邀请码生成与验证模块**
  - AI 指令：在 `backend/src/handlers/invitations.rs` 实现：(1) 邀请码生成器：使用 `rand::rngs::OsRng` 产生 20 位字符（base62 字符集），格式 `INV-{20chars}`；(2) `hash_code(plain) -> String`：SHA-256 后 hex；(3) DAO：插入仅含 `code_hash`、`code_prefix`(明文前 8 位)、`created_by`、`expires_at`，**绝不存明文**
  - 验收标准：单元测试：100 次生成无重复；hash 长度 64；查询时按 hash 命中

-- [14] [x] **邀请码管理 API（管理员）**
  - AI 指令：实现端点（要求管理员中间件鉴权，role='admin'）：
    - `POST /api/invitations/generate` { count, notes?, expires_in_days? } → 返回 `{ invitations: [{id, code, code_prefix, expires_at}], download_url }`，明文 code 仅本次响应返回
    - `GET /api/invitations` 支持 `?status&page&limit`，返回 masked 视图（`INV-AbCd****`），**绝不返回 code_hash**
    - `GET /api/invitations/stats` 返回 `{ total, used, unused, expired }`
    - `DELETE /api/invitations/:id` 按 UUID 撤销，标记 status='revoked'
  - 验收标准：非管理员调用全部返回 403；列表接口响应中无 `code_hash` 字段

-- [15] [x] **邀请码 CSV 一次性下载链接**
  - AI 指令：在生成端点中，将刚生成的明文邀请码列表写入 Redis 临时键（TTL 10 分钟），返回带签名 token 的 `download_url`；实现 `GET /api/invitations/download/:token` 一次性返回 CSV 后立即删除 Redis 键
  - 验收标准：同一 token 第二次访问返回 410 Gone

-- [16] [x] **邀请码公开验证端点**
  - AI 指令：实现 `POST /api/invitations/validate` 接收明文 code（请求体，禁止走 URL），返回 `{ valid, expires_at?, used_by? }`；不返回任何敏感信息；带速率限制（每 IP 30/分钟）
  - 验收标准：无效 code 返回 `{ valid: false }` 不暴露存在性

-- [17] [x] **注册端点**
  - AI 指令：`POST /api/auth/register` 接收 `{ email, password, invitation_code, nickname? }`：
    1. 验证 email 格式、密码强度（≥8 位）
    2. 计算 invitation_code 的 SHA-256，查 invitations 表 status='unused' 且未过期
    3. 在事务中：插入 users（bcrypt 加密密码，自动分配 10 位 account_code，唯一性重试）→ 插入 user_roles(role='user') → 更新 invitations 为 'used' 并填 used_by/used_at
    4. 签发 access_token + refresh_token + csrf_token cookies，响应体 **不返回任何 token**
  - 验收标准：成功 201，响应 Set-Cookie 包含 3 个 cookie；邀请码已用过返回 409

-- [18] [x] **登录端点**
  - AI 指令：`POST /api/auth/login` 接收 `{ email, password }`：bcrypt 验证 → 写入三个 cookie；登录失败 5 次锁定该邮箱 15 分钟（Redis 计数）；速率限制 10/分钟/IP
  - 验收标准：错误密码不暴露 email 是否存在（统一返回 401 "邮箱或密码错误"）

-- [19] [x] **刷新与登出端点**
  - AI 指令：`POST /api/auth/refresh` 读取 refresh_token cookie，验证 + 检查 Redis 白名单（支持服务端撤销），下发新 access_token + csrf_token；`POST /api/auth/logout` Set-Cookie 清空三个 cookie 且从 Redis 撤销 refresh_token
  - 验收标准：登出后 refresh 端点返回 401

-- [20] [x] **管理员 seed 脚本**
  - AI 指令：实现 `backend/src/bin/seed_admin.rs`，CLI 接收 `--email --password`，创建 users 行 + user_roles(role='admin')；若已存在管理员则报错退出
  - 验收标准：`cargo run --bin seed_admin -- --email a@b.c --password XXX` 成功创建管理员，重复运行报错

---

## 阶段 3：用户与好友模块

-- [21] [x] **用户档案 API**
  - AI 指令：实现 ARCHITECTURE.md 第 4.2 节列出的端点：`GET /api/users/me`、`PUT /api/users/me`（昵称、签名、可见性）、`GET /api/users/:user_id`（公开信息）、`GET /api/users/by-code/:code`（按账号码查找）；输入用 Zod 风格 schema 校验
  - 验收标准：未登录调用 `/api/users/me` 返回 401；可见性 false 的用户对非好友返回 404

-- [22] [x] **头像上传与对象存储集成**
  - AI 指令：实现 `POST /api/users/avatar`：接受 multipart/form-data（PNG/JPG ≤ 2MB），上传到 MinIO/S3 桶 `avatars/{user_id}/{uuid}.{ext}`，更新 users.avatar_url；同时生成 200x200 缩略图存到 `avatars/{user_id}/{uuid}_thumb.{ext}`
  - 验收标准：上传后 `users.avatar_url` 指向有效 URL，浏览器可访问

-- [23] [x] **管理员用户管理 API**
  - AI 指令：实现 `GET /api/users`（管理员，分页）和 `PUT /api/users/:user_id/role`（管理员，修改角色）
  - 验收标准：非管理员 403；管理员可将其他用户提升为 admin

-- [24] [x] **好友请求 API**
  - AI 指令：实现 ARCHITECTURE.md 第 4.3 节端点：`POST /api/friends/requests`（按账号码或 user_id，检查不能给自己、不能重复发 pending）、`GET /api/friends/requests/pending`、`PUT /api/friends/requests/:id`（accept 时事务创建 friends 记录，确保 user_id_1 < user_id_2）
  - 验收标准：accept 后双方 `GET /api/friends` 都能看到对方；重复发 pending 返回 409

-- [25] [x] **好友列表与删除 API**
  - AI 指令：`GET /api/friends`（按昵称、最近聊天排序）、`DELETE /api/friends/:friend_id`（删除 friends 行，**消息保留**不删除）
  - 验收标准：删除后双方列表都不再包含对方；历史消息仍可查询

---

## 阶段 4：实时通讯（WebSocket）

-- [26] [x] **WebSocket 握手与鉴权**
  - AI 指令：实现 `GET /api/ws` 升级到 WebSocket（用 `actix_ws::handle(&req, body)` 拿到 `(HttpResponse, Session, MessageStream)`）；握手时从 cookie 读取 `access_token`，验证 JWT；升级成功后启动一个 tokio task 处理入站流，将用户加入 Redis Set `online_users`，并在 task 内维持心跳定时器
  - 验收标准：未登录连接返回 401；登录后连接成功，Redis 中可见 user_id

-- [27] [x] **在线状态广播**
  - AI 指令：用户上线/下线时，查询其好友列表，通过进程内 Session 注册表（`DashMap<UserId, Vec<Sender>>`，每个 Session task 启动时插入 mpsc Sender、退出时移除）向每个在线好友的 Session 推送 `user_online` / `user_offline` 事件；跨实例使用 Redis Pub/Sub 中转
  - 验收标准：A 上线时 A 的所有在线好友都能收到 user_online 事件

-- [28] [x] **消息收发与持久化**
  - AI 指令：处理 client → server 的 `message` 事件：(1) 验证发送方与接收方为好友 (2) 校验内容长度 ≤ 5000 (3) 在事务中插入 messages 行（含 emoji 引用插入 message_emojis） (4) 向发送方回 `message_received` 含 message_id (5) 若接收方在线推送 `message`；离线则入 Redis List `offline_messages:{user_id}`
  - 验收标准：A 发消息给在线 B 延迟 < 200ms；B 离线时 A 发的消息在 B 上线后能被 pop 到

-- [29] [x] **离线消息推送**
  - AI 指令：用户上线时检查 `offline_messages:{user_id}` Redis List，全部 LPOP 后通过 WebSocket 推送，推送成功后删除 Redis 列表
  - 验收标准：B 离线期间收到 5 条消息，B 上线后 5 条全部到达且顺序正确

-- [30] [x] **WebSocket 错误处理与心跳**
  - AI 指令：实现心跳（30s ping/pong），15s 内未响应断开连接；定义 ARCHITECTURE.md 第 4.7 节的 `error` 事件结构，所有失败场景统一通过 error 事件返回
  - 验收标准：客户端断网 30s 后服务端自动结束对应的 Session task，并从 Session 注册表与 Redis online 集合中清理

---

## 阶段 5：消息记录与表情

-- [31] [x] **消息历史查询 API**
  - AI 指令：`GET /api/messages/:friend_id?cursor&limit=50`：基于游标分页，按 `(LEAST(from,to), GREATEST(from,to), created_at)` 双向匹配；利用 ARCHITECTURE.md 中的 `idx_messages_conversation` 复合索引
  - 验收标准：1 万条消息历史查询响应 < 500ms

-- [32] [x] **消息搜索 API**
  - AI 指令：`GET /api/messages/search?q=&from=&to=&date_from=&date_to=`：基于 PostgreSQL full-text search（创建 GIN 索引于 `content` 上的 to_tsvector('simple', content)），返回带高亮片段
  - 验收标准：关键词命中时返回上下文片段；空 q 返回 400

-- [33] [x] **消息软删除**
  - AI 指令：`DELETE /api/messages/:message_id`：仅发送者可删除（4xx 否则），将 `is_deleted=true`；查询接口默认过滤 is_deleted
  - 验收标准：删除后双方都看不到该消息内容（占位"消息已撤回"）

-- [34] [x] **聊天记录导出**
  - AI 指令：`POST /api/messages/export` { friend_id, format: 'json'|'csv', date_from?, date_to? }：流式生成文件，临时存对象存储，返回一次性下载链接（TTL 30 分钟）
  - 验收标准：导出 1000 条消息文件结构正确，能正常下载

-- [35] [x] **表情上传 API**
  - AI 指令：`POST /api/emojis` 接 multipart：PNG/JPG ≤ 500KB；存对象存储 `emojis/{user_id}/{uuid}.{ext}`，生成 100x100 缩略图；同时检查用户表情数 ≤ 100，超出返回 409
  - 验收标准：上传成功返回 emoji 元数据；超过 100 个返回 409

-- [36] [x] **表情查询与删除**
  - AI 指令：`GET /api/emojis` 返回当前用户表情列表（含缩略图 URL，Redis 缓存 60s）；`DELETE /api/emojis/:id` 删除 DB 行 + 对象存储文件
  - 验收标准：删除后再次 GET 不再返回该项；对象存储中文件已清理

---

## 阶段 6：前端实现

-- [37] [x] **认证页面（注册/登录）**
  - AI 指令：实现 `app/(auth)/register/page.tsx` 与 `login/page.tsx`：表单使用 React Hook Form + Zod 校验；注册前调用 `/api/invitations/validate` 提前校验邀请码；登录成功后 router.push('/chat')；fetch 必须 `credentials: 'include'`
  - 验收标准：表单错误提示友好；成功后跳转主应用

-- [38] [x] **CSRF 与 fetch 封装**
  - AI 指令：完善 `frontend/lib/api.ts`：自动从 `csrf_token` cookie 读取并附加 `X-CSRF-Token` 头到所有 mutation；统一错误处理（401 自动调用 refresh）
  - 验收标准：所有受保护 API 调用自动带 CSRF；401 后自动刷新一次再重试

-- [39] [x] **主布局与导航**
  - AI 指令：实现 `app/(app)/layout.tsx`：左侧好友列表 + 右侧聊天窗口的双栏布局（移动端响应式为单栏 + 抽屉）；顶部头像下拉（个人中心、登出）
  - 验收标准：1024px 以上双栏；< 768px 单栏抽屉

-- [40] [x] **好友列表组件**
  - AI 指令：实现 `components/ContactList.tsx`：使用 TanStack Query 拉取 `/api/friends`，显示头像、昵称、在线状态点；支持搜索；点击切换聊天对象
  - 验收标准：在线状态实时更新（监听 WebSocket 事件）

-- [41] [x] **WebSocket 客户端封装**
  - AI 指令：完善 `lib/websocket.ts`：单例连接 `wss://.../api/ws`（cookie 自动携带）；自动重连（指数退避，最大 30s）；提供 `send(event, data)` 与基于 EventEmitter 的 `on(event, handler)`；与 React Context 集成
  - 验收标准：断网恢复后能自动重连；多组件订阅同一事件互不干扰

-- [42] [x] **聊天窗口组件**
  - AI 指令：实现 `components/ChatWindow.tsx`：消息列表（虚拟滚动，react-virtuoso）+ 输入框；接收实时消息后乐观更新 + TanStack Query 缓存；@mention 弹出好友候选；输入框右侧表情按钮
  - 验收标准：1000 条历史消息滚动流畅；新消息到达自动滚到底部（除非用户向上翻看）

-- [43] [x] **历史消息加载与搜索**
  - AI 指令：聊天窗口顶部触底（向上滚）触发 `/api/messages/:friend_id?cursor=` 加载更多；顶部搜索按钮打开搜索面板调用 `/api/messages/search`，命中跳转到对应消息
  - 验收标准：无限滚动正常；搜索结果点击能定位到原消息

-- [44] [x] **表情选择器**
  - AI 指令：`components/EmojiPicker.tsx`：网格布局展示用户表情，支持上传新表情、删除；点击插入到聊天输入框；与 `/api/emojis` 联动
  - 验收标准：上传后立即出现在选择器；插入到输入框的表情在发送后正确渲染

-- [45] [x] **个人档案与头像上传**
  - AI 指令：实现 `app/(app)/profile/page.tsx`：编辑昵称、签名、可见性；头像上传带裁剪预览
  - 验收标准：保存后头像在好友列表中实时刷新

-- [46] [x] **管理员邀请码界面**
  - AI 指令：实现 `app/(app)/admin/invitations/page.tsx`（仅 admin 可见）：批量生成、列表（masked 视图）、统计、撤销；生成成功立即弹窗显示明文 + 一次性 CSV 下载链接，关闭后无法再取回
  - 验收标准：非 admin 路由跳转到 403 页；下载 CSV 后再次访问 url 报错

---

## 阶段 7：测试、文档与部署

-- [47] [ ] **后端单元测试 ≥ 80%**
  - AI 指令：为 handlers 与 db 模块补充 sqlx 集成测试（用 testcontainers 起 postgres）；为 jwt、邀请码 hash、密码 bcrypt 写纯单元测试；目标行覆盖率 ≥ 80%
  - 验收标准：`cargo tarpaulin` 输出 ≥ 80%

-- [48] [ ] **前端单元测试 ≥ 60%**
  - AI 指令：使用 Vitest + React Testing Library 测试关键组件（ChatWindow、ContactList、EmojiPicker、表单校验）；mock fetch 与 WebSocket
  - 验收标准：`npm run test:coverage` 输出 ≥ 60%

-- [49] [ ] **端到端测试（关键流程）**
  - AI 指令：使用 Playwright 写 e2e：(1) 注册（含邀请码）→ 登录 → 添加好友 → 发消息 (2) 管理员生成邀请码并使用 (3) 上传表情并在聊天中使用
  - 验收标准：3 条 e2e 全绿

-- [50] [ ] **生成 docs/API.md**
  - AI 指令：基于代码中实际端点生成 OpenAPI 3.0 spec，转 Markdown；每个端点附 curl 示例（含 cookie/CSRF 头）和成功/失败响应
  - 验收标准：所有 ARCHITECTURE.md 中提到的端点都有文档

-- [51] [ ] **生成 docs/DATABASE.md**
  - AI 指令：导出 PostgreSQL schema 与索引说明；附 ER 图（Mermaid）；说明部分唯一索引 `uq_friend_requests_pending` 的设计意图
  - 验收标准：包含全部表的字段说明、索引清单、约束说明

-- [52] [ ] **生成 docs/DEPLOYMENT.md**
  - AI 指令：编写生产部署指南：Nginx 配置（反向代理 + WSS + SSL）、Docker 镜像构建（多阶段）、环境变量清单、首次启动 seed 管理员、备份策略；目标 30 分钟内可完成自部署
  - 验收标准：实习生按文档步骤可独立完成部署

-- [53] [ ] **生产 Dockerfile（多阶段）**
  - AI 指令：编写 `Dockerfile.frontend`（builder：npm run build；runner：node:20-alpine + standalone output）和 `Dockerfile.backend`（builder：rust:1.70 + cargo build --release；runner：debian:slim）；目标镜像 < 200MB
  - 验收标准：镜像大小达标，容器内能正常启动

-- [54] [ ] **WebSocket 断线重连压力测试**
  - AI 指令：编写脚本模拟 1000 个并发 WebSocket 连接，随机断开 + 重连，验证服务端 Redis online 集合的清理与广播一致性；定位并修复发现的问题
  - 验收标准：1000 并发下消息延迟 P95 < 200ms，无内存泄漏

-- [55] [ ] **上线前安全自检**
  - AI 指令：运行检查清单：(1) 所有 token 仅在 httpOnly cookie (2) 数据库无明文邀请码 (3) 所有 mutation 经 CSRF 中间件 (4) bcrypt cost ≥ 12 (5) JWT 使用 RS256 (6) 限流生效 (7) 错误响应不暴露内部细节
  - 验收标准：全部 7 项打勾；输出报告到 `docs/SECURITY_CHECKLIST.md`

---

*最后更新：2026-05-07（前端阶段六完成 — 聊天/搜索/表情/管理员）*
*下一个待处理任务：[47] 后端单元测试 ≥ 80%*
