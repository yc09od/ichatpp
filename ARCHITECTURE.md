# 技术架构 — ichatpp

## 1. 架构概览

ichatpp 采用**分离的前后端架构**，前端使用 Next.js 提供现代化的用户界面，后端使用 Rust 实现高性能的服务器。两端通过 REST API 和 WebSocket 进行通讯。数据持久化使用 PostgreSQL，实时缓存使用 Redis。图片存储采用对象存储（MinIO 或 S3）以支持扩展。

### 架构图

```
┌─────────────────────────────────────────────────────────────┐
│                      Client (Browser)                        │
│                      ┌──────────────┐                        │
│                      │   Next.js    │ (Tailwind CSS)         │
│                      │   Frontend   │                        │
│                      └──────┬───────┘                        │
└──────────────────────────────┼────────────────────────────────┘
         HTTP/REST            │         WebSocket (WSS)
                              │
    ┌─────────────────────────┴─────────────────────────────┐
    │                                                       │
┌──▼────────────────────────────────────────────────────────▼──┐
│                   Backend (Rust Server)                       │
│                   ┌──────────────────────────┐               │
│                   │   Actix-web Framework    │               │
│                   │  ┌────────────────────┐  │               │
│                   │  │ API Handlers (REST)│  │               │
│                   │  │ WebSocket Handler  │  │               │
│                   │  │ Auth & Middleware  │  │               │
│                   │  └────────────────────┘  │               │
│                   └──────────────────────────┘               │
└────┬──────────────────┬──────────────────────┬───────────────┘
     │                  │                      │
     │                  │                      │
┌────▼──┐      ┌───────▼──────┐      ┌────────▼──┐
│ Postgre│      │    Redis     │      │  MinIO/S3 │
│  SQL   │      │  (Cache)     │      │  (Images) │
│(Data)  │      └──────────────┘      └───────────┘
└────────┘
```

## 2. 技术栈

### 前端

| 技术 | 版本 | 用途 |
|------|------|------|
| Next.js | 14.x | React 框架，支持 SSR、API Routes |
| React | 18.x | UI 组件库 |
| TypeScript | 5.x | 类型安全的 JavaScript |
| Tailwind CSS | 3.x | 工具类 CSS 框架 |
| TanStack Query | 5.x | 数据获取、缓存、mutation 与乐观更新 |
| Zod | latest | 运行时数据验证 |
| 浏览器原生 WebSocket API | — | WebSocket 客户端（不使用 Socket.io，与后端协议保持一致） |

### 后端

| 技术 | 版本 | 用途 |
|------|------|------|
| Rust | 1.70+ | 编程语言 |
| Actix-web | 4.x | 异步 Web 框架 |
| Tokio | 1.x | 异步运行时 |
| Serde | latest | 序列化/反序列化 |
| Serde JSON | latest | JSON 处理 |
| SQLx | latest | 异步 SQL 查询构建器 |
| Redis | crate | Redis 客户端 |
| Jsonwebtoken | latest | JWT 认证 |
| Bcrypt | latest | 密码加密 |
| Uuid | latest | UUID 生成 |
| actix-web-actors | 4.x | WebSocket 支持（基于 actor 模型，Actix 官方组件） |
| Env_logger | latest | 日志框架 |

### 数据库

| 技术 | 版本 | 用途 |
|------|------|------|
| PostgreSQL | 14+ | 主数据库（用户、好友、消息） |
| Redis | 6+ | 会话缓存、在线状态、消息队列 |
| MinIO / AWS S3 | latest | 对象存储（用户头像、表情图片） |

### 基础设施 / 部署

| 技术 | 版本 | 用途 |
|------|------|------|
| Docker | 20+ | 容器化 |
| Docker Compose | 2.x | 本地开发环境编排 |
| Nginx | 1.21+ | 反向代理、静态文件服务 |
| GitHub Actions | - | CI/CD 流程 |

## 3. 目录结构

```
ichatpp/
├── frontend/                    # Next.js 前端项目
│   ├── app/                    # Next.js app 目录
│   │   ├── (auth)/             # 认证相关页面
│   │   │   ├── login/
│   │   │   └── register/
│   │   ├── (app)/              # 主应用页面
│   │   │   ├── chat/           # 聊天页面
│   │   │   ├── contacts/       # 好友列表
│   │   │   ├── profile/        # 用户档案
│   │   │   └── layout.tsx
│   │   └── api/                # API Routes (Next Auth 等)
│   ├── components/             # React 组件
│   │   ├── ChatWindow.tsx
│   │   ├── ContactList.tsx
│   │   ├── EmojiPicker.tsx
│   │   └── ...
│   ├── lib/                    # 工具函数
│   │   ├── api.ts              # API 调用封装
│   │   ├── websocket.ts        # WebSocket 连接管理
│   │   └── ...
│   ├── styles/                 # 全局样式
│   ├── package.json
│   └── tsconfig.json
│
├── backend/                     # Rust 后端项目
│   ├── src/
│   │   ├── main.rs             # 应用入口
│   │   ├── models/             # 数据模型
│   │   │   ├── user.rs
│   │   │   ├── message.rs
│   │   │   └── ...
│   │   ├── handlers/           # HTTP 请求处理
│   │   │   ├── auth.rs
│   │   │   ├── user.rs
│   │   │   ├── message.rs
│   │   │   └── ws.rs           # WebSocket 处理
│   │   ├── db/                 # 数据库操作
│   │   │   ├── mod.rs
│   │   │   └── queries/
│   │   ├── middleware/         # 中间件（认证、日志等）
│   │   ├── errors/             # 错误定义
│   │   └── config.rs           # 配置管理
│   ├── Cargo.toml
│   ├── Cargo.lock
│   ├── migrations/             # SQL 迁移文件
│   └── .env.example
│
├── docs/                        # 文档
│   ├── API.md                  # API 文档
│   ├── DATABASE.md             # 数据库 schema
│   ├── DEPLOYMENT.md           # 部署指南
│   └── ...
│
├── docker-compose.yml          # 本地开发 Docker 编排
├── Dockerfile.frontend         # 前端 Docker 镜像
├── Dockerfile.backend          # 后端 Docker 镜像
├── .github/                    # GitHub Actions 配置
│   └── workflows/
│       ├── frontend-ci.yml
│       └── backend-ci.yml
│
└── README.md
```

## 4. 核心模块设计

### 4.1 认证模块（Auth）

**职责：** 用户注册、登录、Token 生成与验证、邀请码管理

**技术方案：**
- 使用 JWT (JSON Web Token) 作为认证令牌
- 密码存储使用 bcrypt（成本因子 12）
- 刷新令牌存储在 Redis（用于支持服务端撤销），有效期 7 天
- 访问令牌有效期 1 小时
- **Token 存储：通过后端 Set-Cookie 写入 httpOnly + Secure + SameSite=Lax cookie，前端 JavaScript 不接触 token**
  - `access_token` cookie：httpOnly, Secure, SameSite=Lax, Max-Age=3600, Path=/
  - `refresh_token` cookie：httpOnly, Secure, SameSite=Lax, Max-Age=604800, Path=/api/auth/refresh
  - `csrf_token` cookie：**非 httpOnly**（前端需读取），Secure, SameSite=Lax, Max-Age=3600, Path=/
- **CSRF 防护**：双提交 cookie 方案——前端读取 csrf_token cookie 后在受保护请求的 `X-CSRF-Token` 头中回传，后端比对一致
- 邀请码使用 SHA-256 哈希存储，生成时使用 `OsRng` 强随机；明文仅在生成响应中一次性返回
- 邀请码有效期 7 天，支持手动过期

**注册流程：**
1. 用户输入邮箱、邀请码（明文）
2. 后端对邀请码做 SHA-256 后查 `code_hash`，验证 status='unused' 且未过期
3. 用户输入密码
4. 在事务中：创建账户、分配账号码、标记邀请码为 'used'
5. 通过 Set-Cookie 写入 access_token + refresh_token + csrf_token，响应体不返回任何 token 字段

**管理员邀请码生成流程：**
1. 管理员登录（JWT 必须包含 admin 角色）
2. 指定生成数量和备注
3. 后端生成邀请码列表（16 进制 + 特殊字符混合）
4. 返回邀请码列表，管理员可下载为 CSV

**API 端点：**
```
POST   /api/auth/register                      # 注册（需要 invitation_code；成功设置 access_token + refresh_token cookies）
POST   /api/auth/login                         # 登录（成功设置 access_token + refresh_token + csrf_token cookies）
POST   /api/auth/refresh                       # 刷新 access token（读取 refresh_token cookie，下发新 access_token cookie）
POST   /api/auth/logout                        # 登出（Set-Cookie 清除全部认证 cookie）
POST   /api/invitations/validate               # 验证邀请码（明文走请求体，不走 URL，防访问日志泄露）
POST   /api/invitations/generate               # 生成邀请码（仅管理员）
GET    /api/invitations                        # 获取邀请码列表（仅管理员，返回 code_prefix + masked，不返回 code_hash）
GET    /api/invitations/stats                  # 邀请码统计（仅管理员）
DELETE /api/invitations/:id                    # 撤销邀请码（仅管理员，按数据库 UUID）
```

### 4.2 用户模块（User）

**职责：** 用户信息管理、档案编辑、账号码分配、角色管理

**关键功能：**
- 用户注册时自动分配唯一账号码（10 位数字）
- 用户可编辑：昵称、头像、个性签名、可见性（隐私设置）
- 头像和表情存储到对象存储（MinIO/S3）
- 生成缩略图版本以优化加载
- 系统自动为新用户分配 `user` 角色，仅管理员可修改

**API 端点：**
```
GET    /api/users/me                # 获取当前用户信息（包含角色）
PUT    /api/users/me                # 更新用户档案
GET    /api/users/:user_id          # 获取用户公开信息
GET    /api/users/by-code/:code     # 按账号码搜索用户
POST   /api/users/avatar            # 上传头像
GET    /api/users                   # 获取所有用户（仅管理员）
PUT    /api/users/:user_id/role     # 修改用户角色（仅管理员）
```

### 4.3 好友模块（Friends）

**职责：** 好友请求、好友列表、好友删除

**数据模型：**
```
friend_requests:
  - id (UUID)
  - from_user_id
  - to_user_id
  - status (pending, accepted, rejected)
  - created_at

friends:
  - id (UUID)
  - user_id_1
  - user_id_2
  - created_at
```

**关键逻辑：**
- 发送好友请求时检查是否已为好友
- 接受好友请求后创建双向好友关系
- 删除好友时清空相关对话历史（可选）

**API 端点：**
```
POST   /api/friends/requests         # 发送好友请求
GET    /api/friends/requests/pending # 获取待处理请求
PUT    /api/friends/requests/:id     # 接受/拒绝好友请求
GET    /api/friends                  # 获取好友列表
DELETE /api/friends/:friend_id       # 删除好友
```

### 4.4 消息模块（Message）

**职责：** 消息持久化、历史查询、消息搜索

**数据模型：**
```
messages:
  - id (UUID)
  - from_user_id
  - to_user_id
  - content (TEXT)
  - emoji_ids (Array<UUID>) 表情引用
  - created_at
  - updated_at
  - is_deleted
```

**消息流程：**
1. 前端通过 WebSocket 发送消息
2. 后端接收 → 验证 → 保存到 DB
3. 后端发送确认给发送方
4. 后端广播给接收方（如在线）
5. 离线消息等接收方上线时推送

**API 端点：**
```
GET    /api/messages/:friend_id      # 获取与某好友的消息历史
GET    /api/messages/search          # 搜索消息（关键词、日期范围）
DELETE /api/messages/:message_id     # 删除消息（软删除）
POST   /api/messages/export          # 导出聊天记录
```

### 4.5 表情模块（Emoji）

**职责：** 用户自定义表情管理和存储

**数据模型：**
```
emojis:
  - id (UUID)
  - user_id
  - name (VARCHAR)
  - file_url (S3/MinIO 路径)
  - thumbnail_url (缩略图)
  - mime_type (image/png, image/jpeg)
  - file_size
  - created_at
```

**关键特性：**
- 支持上传 PNG、JPG（最大 500KB）
- 自动生成 100x100 像素缩略图
- 缓存在 Redis 中，加快加载
- 删除时清理 S3/MinIO 中的文件

**API 端点：**
```
POST   /api/emojis                   # 上传表情
GET    /api/emojis                   # 获取我的表情库
DELETE /api/emojis/:emoji_id         # 删除表情
```

### 4.6 邀请码管理模块（Invitation）

**职责：** 邀请码生成、验证、状态管理（仅管理员使用）

**权限控制：**
- 所有邀请码管理端点都需要管理员权限（JWT payload 中 role = "admin"）
- 非管理员调用这些端点返回 403 Forbidden

**邀请码生成逻辑：**
1. 生成 20 位随机字符串（使用强随机源）
2. 格式为 `INV-{20个字符}`（便于识别）
3. SHA-256 哈希后存储到数据库（只存哈希值，出于安全性）
4. 设定过期时间为 7 天后
5. 记录生成者和生成时间

**邀请码验证流程（用户注册时）：**
1. 用户提交邮箱 + 邀请码（明文）+ 密码
2. 后端对明文计算 SHA-256，得到 hex 字符串
3. 查询 invitations 表 WHERE code_hash = ?
4. 检查：
   - 哈希是否命中
   - 状态是否为 'unused'
   - 是否超过过期时间
5. 如果验证成功，创建用户账户，标记邀请码为 'used'，记录使用者和使用时间
6. 如果验证失败，返回具体错误信息

**存储约定：**
- 数据库永远不保存明文邀请码
- 明文仅在 POST /api/invitations/generate 的响应中一次性返回（连同一次性 download_url 提供 CSV）
- 生成后明文不可再从服务端取回，管理员需妥善保管下载的 CSV

**API 端点：**
```
POST   /api/invitations/generate        # 生成邀请码（仅管理员）
  请求：{ count: 10, notes: "Beta 测试用户", expires_in_days?: 7 }
  响应：{
    invitations: [{ id, code: "INV-xxx", code_prefix: "INV-AbCd", expires_at }],
    download_url: "..."
  }
  注意：明文 code 仅本次响应返回，数据库只存 code_hash

GET    /api/invitations                 # 列出所有邀请码（仅管理员）
  查询参数：?status=unused&page=1&limit=50
  响应：[{ id, code_prefix: "INV-AbCd", masked: "INV-AbCd****", status,
           used_by, created_at, expires_at, notes }]
  注意：不返回 code_hash 字段（避免离线碰撞攻击）；前端展示用 masked

GET    /api/invitations/stats           # 邀请码统计（仅管理员）
  响应：{ total: 100, used: 50, unused: 40, expired: 10 }

DELETE /api/invitations/:id             # 撤销邀请码（仅管理员，按数据库 UUID）
  功能：将邀请码标记为 'revoked'，后续无法使用
  注意：因明文不再保存，必须按 invitations.id 操作而非按 code

GET    /api/invitations/validate        # 验证邀请码（任何人可调用，用于前端注册前提前检查）
  请求体：{ code: "INV-xxx" }（明文，POST 体或加密查询参数；不要走 URL path 防泄露日志）
  响应：{ valid: true, expires_at: "...", used_by: null }
```

### 4.7 WebSocket 模块（Real-time Communication）

**职责：** 实时消息传输、在线状态管理

**连接生命周期：**
1. 客户端发起 WebSocket 升级时，浏览器自动携带 `access_token` cookie；后端在握手阶段验证 JWT
2. 服务器添加用户到在线集合（Redis Set）
3. 广播用户上线事件给所有好友
4. 接收消息 → 存储 → 广播
5. 断开连接 → 移除用户 → 广播离线事件

**事件类型：**
```
client -> server:
  - message: { content, emoji_ids, to_user_id }
  - typing: { to_user_id }
  - read: { message_id }

server -> client:
  - message_received: { message_id, timestamp }
  - user_online: { user_id }
  - user_offline: { user_id }
  - message: { from_user_id, content, emoji_ids, created_at }
  - error: { code, message }
```

**性能优化：**
- 使用 Redis Pub/Sub 实现跨实例消息广播
- 对于离线用户，消息入队到 Redis List
- 用户上线时弹出待推送消息

## 5. 数据模型

```sql
-- 用户表（登录凭证仅 email；nickname 作为显示名；不再使用 username 字段）
CREATE TABLE users (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  email VARCHAR(255) NOT NULL UNIQUE,
  password_hash VARCHAR(255) NOT NULL,
  account_code CHAR(10) NOT NULL UNIQUE CHECK (account_code ~ '^[0-9]{10}$'),
  nickname VARCHAR(100),
  avatar_url VARCHAR(255),
  signature TEXT,
  is_visible BOOLEAN DEFAULT TRUE,
  created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- 好友表
CREATE TABLE friends (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id_1 UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  user_id_2 UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(user_id_1, user_id_2),
  CHECK (user_id_1 < user_id_2)
);

-- 好友请求表
-- 注意：未使用表级 UNIQUE 约束，改用部分唯一索引（见下文），
-- 以允许被拒绝/接受后再次发起请求，同时防止同时存在多条 pending 请求
CREATE TABLE friend_requests (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  from_user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  to_user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  status VARCHAR(20) DEFAULT 'pending', -- pending, accepted, rejected
  created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- 仅对 pending 状态保持唯一，已完成（accepted/rejected）的历史记录可保留
CREATE UNIQUE INDEX uq_friend_requests_pending
  ON friend_requests(from_user_id, to_user_id)
  WHERE status = 'pending';

-- 消息表
CREATE TABLE messages (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  from_user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  to_user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  content TEXT NOT NULL,
  is_deleted BOOLEAN DEFAULT FALSE,
  created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- 表情表（必须在 message_emojis 之前创建以满足外键依赖）
CREATE TABLE emojis (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  name VARCHAR(100) NOT NULL,
  file_url VARCHAR(255) NOT NULL,
  thumbnail_url VARCHAR(255),
  mime_type VARCHAR(50),
  file_size INT,
  created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- 消息 - 表情关联表（emoji_id 添加外键，避免孤儿引用）
CREATE TABLE message_emojis (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  message_id UUID NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
  emoji_id UUID NOT NULL REFERENCES emojis(id) ON DELETE CASCADE,
  position INT NOT NULL
);

-- 用户角色表
CREATE TABLE user_roles (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id UUID NOT NULL UNIQUE REFERENCES users(id) ON DELETE CASCADE,
  role VARCHAR(50) DEFAULT 'user', -- user, admin
  created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- 邀请码表（仅存哈希；明文仅在生成响应中一次性返回，事后无法找回）
CREATE TABLE invitations (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  code_hash CHAR(64) NOT NULL UNIQUE,    -- SHA-256(plaintext) 的十六进制（64 字符）
  code_prefix VARCHAR(8),                -- 明文前 8 位（如 'INV-AbCd'），仅供管理员识别"哪一批"，不参与验证
  created_by UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  used_by UUID REFERENCES users(id) ON DELETE SET NULL,
  status VARCHAR(20) DEFAULT 'unused',   -- unused, used, expired, revoked
  created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
  used_at TIMESTAMP,
  expires_at TIMESTAMP DEFAULT (CURRENT_TIMESTAMP + INTERVAL '7 days'),
  notes VARCHAR(255)
);

-- 索引
CREATE INDEX idx_users_account_code ON users(account_code);
CREATE INDEX idx_users_role ON user_roles(user_id);

-- 消息查询常用模式：按双方 + 时间倒序拉取聊天记录，需要双向复合索引
CREATE INDEX idx_messages_conversation
  ON messages(from_user_id, to_user_id, created_at DESC);
CREATE INDEX idx_messages_conversation_reverse
  ON messages(to_user_id, from_user_id, created_at DESC);
CREATE INDEX idx_messages_created_at ON messages(created_at);

CREATE INDEX idx_friends_user1 ON friends(user_id_1);
CREATE INDEX idx_friends_user2 ON friends(user_id_2);
CREATE INDEX idx_invitations_code_hash ON invitations(code_hash);
CREATE INDEX idx_invitations_status ON invitations(status);
CREATE INDEX idx_invitations_created_by ON invitations(created_by);
CREATE INDEX idx_invitations_expires_at ON invitations(expires_at);
```

## 6. API 设计原则

**风格：** REST API

**认证：** JWT 通过 httpOnly cookie 自动携带（不使用 Authorization Bearer 头）
- 浏览器自动发送 `access_token` cookie 给后端
- 前端 fetch 调用必须 `credentials: 'include'`
- 受保护的 mutation 请求（POST/PUT/DELETE/PATCH）必须附带 `X-CSRF-Token` 头，值取自 csrf_token cookie

**CORS：** 后端必须配置 `Access-Control-Allow-Credentials: true` 与精确的 `Access-Control-Allow-Origin`（不能用通配符 `*`，浏览器规则）

**版本管理：** URL 版本 (`/api/v1/...`)

**错误响应：**
```json
{
  "error": {
    "code": "INVALID_INPUT",
    "message": "邮箱格式不正确",
    "details": { "field": "email" }
  }
}
```

**成功响应：**
```json
{
  "data": { /* ... */ },
  "meta": { "timestamp": "2026-05-07T12:00:00Z" }
}
```

**速率限制：**
- 登录：10 次/分钟（IP）
- API：100 次/分钟（用户）
- WebSocket：1000 消息/小时（用户）

## 7. 部署架构

**开发环境：**
- Docker Compose 本地启动：PostgreSQL、Redis、MinIO、后端、前端
- 前端热重载 (Next.js)
- 后端使用 `cargo watch` 热重载

**生产环境：**
```
Internet
    ↓
CDN (可选)
    ↓
Nginx (反向代理、SSL)
    ↓
Load Balancer (可选，多实例时)
    ↓
Next.js Server (多实例)
Rust Server (多实例)
    ↓
PostgreSQL (主从复制)
Redis (Cluster)
S3/MinIO
```

**容器化：**
- Dockerfile 分多阶段构建（减小镜像体积）
- Docker Compose 定义完整栈
- GitHub Actions 自动构建和推送到镜像仓库

## 8. 安全考虑

**认证与授权：**
- HTTPS/WSS 加密传输
- JWT 签名验证（RS256 非对称加密）
- **Token 存于 httpOnly + Secure + SameSite=Lax cookie**，前端 JS 无法读取 access_token / refresh_token，杜绝 XSS 窃取
- refresh_token cookie 限定 Path=/api/auth/refresh，缩小提交面
- 密码最小 8 字符，建议包含大小写、数字、特殊字符

**数据保护：**
- 敏感字段（密码哈希、令牌）不暴露在 API 响应
- 消息内容在数据库中可选加密
- 定期备份数据库

**输入验证：**
- 所有输入使用 Zod (前端) 和 SQLx (后端) 验证
- SQL 注入防护：使用参数化查询
- XSS 防护：React 自动转义，Server-side 验证；token 已存于 httpOnly cookie，即便 XSS 也无法窃取
- **CSRF 防护**：双提交 cookie——后端发非 httpOnly 的 `csrf_token` cookie，前端在 mutation 请求中以 `X-CSRF-Token` 头回传，中间件比对两者一致才放行

**速率限制与防暴力：**
- 登录失败 5 次后锁定 15 分钟
- API 端点使用令牌桶算法限流
- 异常请求记录日志

## 9. 技术决策记录（ADR）

### ADR-001：为什么后端选择 Rust 而不是 Node.js？

**背景：** 需要选择后端语言

**候选方案：**
- Node.js：开发快，生态丰富，但单线程模型下高并发需要额外优化
- Rust：编译型，天然并发，内存安全，但学习曲线陡峭

**决策：** Rust

**理由：**
1. WebSocket 实时通讯对并发性能要求高
2. Rust 的内存安全保证减少 bug
3. 展示现代系统编程语言的实际应用
4. 部署成本低（单个二进制文件）

**权衡：**
- 开发速度可能较慢，但代码质量更高
- 社区相对小，但通讯库生态足够

---

### ADR-002：为什么使用 PostgreSQL 而不是 MongoDB？

**背景：** 需要选择数据库

**决策：** PostgreSQL

**理由：**
1. 数据结构明确（用户、好友、消息），关系型数据库更自然
2. 支持事务，确保数据一致性（例如好友关系操作）
3. 丰富的索引和查询优化能力，适合消息历史查询
4. 开源，部署成本低

**MongoDB 不选的原因：**
- 文档型适合灵活的数据结构，但我们的结构固定
- 事务支持相对较弱

---

### ADR-003：为什么使用 WebSocket 而不是 HTTP Polling？

**背景：** 实时消息传输方案选择

**决策：** WebSocket

**理由：**
1. 双向实时通讯，延迟 < 200ms
2. 减少网络开销（避免频繁的 HTTP 握手）
3. 连接持久化，更好的用户体验

---

### ADR-004：表情存储方案 - MinIO vs S3 vs 本地文件系统

**决策：** 优先 MinIO（开源自部署），其次 S3（云托管）

**理由：**
1. 开源项目应支持自部署，MinIO 满足此需求
2. S3 是事实标准，易于迁移
3. 避免本地文件系统（扩展困难）

---

*最后更新：2026-05-07（同步 scaffold-project-update：邀请码哈希存储、项目改名 ichatpp、token 改 httpOnly cookies）*
