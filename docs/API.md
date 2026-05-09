# ichatpp API 参考

> 基于代码中实际端点导出（截至 2026-05-07）。涵盖 `backend/src/handlers/*` 与 `backend/src/main.rs` 路由配置。
> 全局响应包络见 [响应格式](#响应格式)；错误码见 [错误响应](#错误响应)。

## 全局约定

- **Base URL**：开发 `http://localhost:8080`，生产 `https://api.chat.example.com`（前端在 `chat.example.com`，后端独立子域，详见 [DEPLOYMENT.md §4](DEPLOYMENT.md)）
- **认证**：JWT 通过 `Set-Cookie` 写入 **httpOnly + Secure + SameSite=Lax** 的 `access_token`/`refresh_token` cookie；前端 JS 不接触 token
- **CSRF**：所有 mutation（POST/PUT/PATCH/DELETE，除 `/api/auth/login` 与 `/api/auth/register`）必须附 `X-CSRF-Token` 请求头，值取自 `csrf_token` cookie（**双提交模式**）
- **Cookie 携带**：浏览器侧必须 `fetch(..., { credentials: 'include' })`
- **Content-Type**：JSON 端点用 `application/json`；上传端点用 `multipart/form-data`
- **CORS**：`Access-Control-Allow-Credentials: true`，origin 由 `FRONTEND_ORIGIN` 配置；禁止 `*`
- **限流**：全局 100 req/min/IP；`/api/auth/*` 10 req/min/IP；`/api/invitations/validate` 30 req/min/IP

## 响应格式

### 成功

```json
{
  "data": <body>,
  "meta": { "timestamp": "2026-05-07T12:34:56Z" }
}
```

### 错误

```json
{
  "error": {
    "code": "NOT_FOUND",
    "message": "user not found",
    "details": null
  }
}
```

| HTTP | code 示例 |
|------|-----------|
| 400 | `BAD_REQUEST` |
| 401 | `UNAUTHORIZED` |
| 403 | `FORBIDDEN` |
| 404 | `NOT_FOUND` |
| 409 | `CONFLICT` |
| 410 | `GONE` |
| 429 | `TOO_MANY_REQUESTS` |
| 500 | `INTERNAL` |

---

## 端点清单

| 分组 | 方法 | 路径 | 鉴权 | CSRF |
|------|------|------|------|------|
| 健康 | GET | `/api/health` | ❌ | — |
| WebSocket | GET | `/api/ws` | ✅ Cookie | — |
| 认证 | POST | `/api/auth/register` | ❌ | ❌ |
| 认证 | POST | `/api/auth/login` | ❌ | ❌ |
| 认证 | POST | `/api/auth/refresh` | ✅ Refresh Cookie | ✅ |
| 认证 | POST | `/api/auth/logout` | ✅ | ✅ |
| 用户 | GET | `/api/users/me` | ✅ | — |
| 用户 | PUT | `/api/users/me` | ✅ | ✅ |
| 用户 | POST | `/api/users/avatar` | ✅ | ✅ |
| 用户 | GET | `/api/users/by-code/{code}` | ✅ | — |
| 用户 | GET | `/api/users/{user_id}` | ✅ | — |
| 用户（admin） | GET | `/api/users` | ✅ admin | — |
| 用户（admin） | PUT | `/api/users/{user_id}/role` | ✅ admin | ✅ |
| 好友 | GET | `/api/friends` | ✅ | — |
| 好友 | POST | `/api/friends/requests` | ✅ | ✅ |
| 好友 | GET | `/api/friends/requests/pending` | ✅ | — |
| 好友 | PUT | `/api/friends/requests/{id}` | ✅ | ✅ |
| 好友 | DELETE | `/api/friends/{friend_id}` | ✅ | ✅ |
| 消息 | GET | `/api/messages/{friend_id}` | ✅ | — |
| 消息 | GET | `/api/messages/search` | ✅ | — |
| 消息 | DELETE | `/api/messages/{message_id}` | ✅ | ✅ |
| 消息 | POST | `/api/messages/export` | ✅ | ✅ |
| 消息 | GET | `/api/messages/download/{token}` | ❌（token 即凭证） | — |
| 表情 | GET | `/api/emojis` | ✅ | — |
| 表情 | POST | `/api/emojis` | ✅ | ✅ |
| 表情 | GET | `/api/emojis/{id}` | ✅ | — |
| 表情 | DELETE | `/api/emojis/{id}` | ✅ | ✅ |
| 邀请码（admin） | POST | `/api/invitations/generate` | ✅ admin | ✅ |
| 邀请码（admin） | GET | `/api/invitations` | ✅ admin | — |
| 邀请码（admin） | GET | `/api/invitations/stats` | ✅ admin | — |
| 邀请码（admin） | DELETE | `/api/invitations/{id}` | ✅ admin | ✅ |
| 邀请码（admin） | GET | `/api/invitations/download/{token}` | ✅ admin | — |
| 邀请码 | POST | `/api/invitations/validate` | ❌ | ❌ |

---

## 健康

### `GET /api/health`

```bash
curl -i http://localhost:8080/api/health
```

```json
{ "status": "ok" }
```

---

## 认证（Auth）

### `POST /api/auth/register`

注册新用户。需要有效未使用的邀请码。

**请求体**

```json
{
  "email": "alice@example.com",
  "password": "Password123",
  "invitation_code": "INV-AbCd...20chars",
  "nickname": "Alice"
}
```

**成功 201**

```bash
curl -i -X POST http://localhost:8080/api/auth/register \
  -H "Content-Type: application/json" \
  -d '{"email":"a@b.c","password":"Password123","invitation_code":"INV-..."}'
```

`Set-Cookie` 三个：`access_token` / `refresh_token` / `csrf_token`。

```json
{
  "data": {
    "user_id": "uuid",
    "account_code": "0123456789"
  },
  "meta": { "timestamp": "..." }
}
```

**错误**

| HTTP | 场景 |
|------|------|
| 400 | email/密码格式不合法、邀请码不存在 |
| 409 | 邀请码已被使用、email 已注册 |

---

### `POST /api/auth/login`

```bash
curl -i -X POST http://localhost:8080/api/auth/login \
  -H "Content-Type: application/json" \
  -c cookies.txt \
  -d '{"email":"a@b.c","password":"Password123"}'
```

**成功 200**：写入 3 个 cookie，body 同 register。

**错误**

| HTTP | 含义 |
|------|------|
| 401 | 邮箱或密码错误（**统一文案**，不区分用户存在与否） |
| 401 | 5 次失败后被 15 分钟锁定（按 email） |

---

### `POST /api/auth/refresh`

```bash
curl -i -X POST http://localhost:8080/api/auth/refresh \
  -b cookies.txt -c cookies.txt
```

**成功 200**：仅写入新的 `access_token` + `csrf_token`，不重发 `refresh_token`。

**错误**：401（缺失 cookie / 签名错误 / 已被撤销 / 已过期 — 统一返回）

---

### `POST /api/auth/logout`

```bash
curl -i -X POST http://localhost:8080/api/auth/logout \
  -b cookies.txt \
  -H "X-CSRF-Token: $(grep csrf_token cookies.txt | awk '{print $7}')"
```

**成功 200**：清空三个 cookie，并从 Redis 撤销该用户**所有**活跃 refresh_token（"全设备登出"语义）。

---

## 用户（Users）

### `GET /api/users/me`

```bash
curl -b cookies.txt http://localhost:8080/api/users/me
```

```json
{
  "data": {
    "id": "uuid",
    "account_code": "0123456789",
    "email": "alice@example.com",
    "nickname": "Alice",
    "avatar_url": "https://.../avatars/uuid.png",
    "signature": "hi",
    "is_visible": true,
    "role": "user",
    "created_at": "..."
  }
}
```

### `PUT /api/users/me`

```bash
curl -X PUT http://localhost:8080/api/users/me \
  -b cookies.txt -H "Content-Type: application/json" \
  -H "X-CSRF-Token: <csrf>" \
  -d '{"nickname":"NewName","signature":"...","is_visible":false}'
```

部分更新：未传字段不变。

### `POST /api/users/avatar`

```bash
curl -X POST http://localhost:8080/api/users/avatar \
  -b cookies.txt -H "X-CSRF-Token: <csrf>" \
  -F "file=@avatar.png"
```

PNG/JPG ≤ 2MB。响应包含 `user`（更新后档案）+ `thumbnail_url`。

### `GET /api/users/{user_id}` / `GET /api/users/by-code/{code}`

- 任意已登录用户可调用
- 目标用户 `is_visible=false` 且双方非好友 → 404（不暴露存在）
- account_code 必须 10 位数字，否则 400

### `GET /api/users` （admin）

```bash
curl -b admin-cookies.txt "http://localhost:8080/api/users?page=1&limit=50"
```

`limit` 上限 100，超出 400。

### `PUT /api/users/{user_id}/role` （admin）

```json
{ "role": "admin" }   // or "user"
```

- 自降级被禁止（admin 不能把自己设为 user）→ 400
- 未知 user_id → 404

---

## 好友（Friends）

### `GET /api/friends`

按"最近聊天"排序。响应：

```json
{
  "data": {
    "friends": [
      {
        "id": "uuid",
        "account_code": "...",
        "nickname": "...",
        "avatar_url": "...",
        "is_visible": true,
        "last_message_at": "..."
      }
    ]
  }
}
```

### `POST /api/friends/requests`

```bash
curl -X POST http://localhost:8080/api/friends/requests \
  -b cookies.txt -H "Content-Type: application/json" \
  -H "X-CSRF-Token: <csrf>" \
  -d '{"account_code":"0123456789"}'
# 或 -d '{"user_id":"<uuid>"}'
```

二选一字段：`account_code` 或 `user_id`。

| HTTP | 场景 |
|------|------|
| 400 | 自加自；两字段都传 / 都没传；account_code 非 10 位数字 |
| 404 | 目标用户不存在 |
| 409 | 已是好友 / 双向已存在 pending |

### `GET /api/friends/requests/pending`

仅返回**入站**待处理请求（带发送方公开信息）。

### `PUT /api/friends/requests/{id}`

```json
{ "action": "accept" }   // or "reject"
```

仅请求的接收方可以 accept/reject。其他人 → 404。

### `DELETE /api/friends/{friend_id}`

删除好友关系（消息历史保留）。删自己 → 400；非好友 → 404。

---

## 消息（Messages）

### `GET /api/messages/{friend_id}`

游标分页。

```bash
curl -b cookies.txt "http://localhost:8080/api/messages/<friend_id>?limit=50&cursor=<base64>"
```

**响应**

```json
{
  "data": {
    "messages": [
      {
        "id": "uuid",
        "from_user_id": "...",
        "to_user_id": "...",
        "content": "...",
        "is_deleted": false,
        "emoji_ids": ["..."],
        "created_at": "..."
      }
    ],
    "next_cursor": "<base64>" 
  }
}
```

`limit` 上限 100；自查 → 400；malformed cursor → 400。

### `GET /api/messages/search`

```bash
curl -b cookies.txt "http://localhost:8080/api/messages/search?q=hello&from=<uuid>&date_from=...&date_to=...&limit=20"
```

PostgreSQL FTS（GIN 索引于 `to_tsvector('simple', content)`）。响应中 `headline` 含 `<mark>` 高亮。

| HTTP | 场景 |
|------|------|
| 400 | q 为空 / 全空白；date_from ≥ date_to；limit > 50 |

### `DELETE /api/messages/{message_id}`

软删除（`is_deleted=true`，content 保留）。

| HTTP | 场景 |
|------|------|
| 204 | 成功 / 已删除（idempotent） |
| 403 | 非发送者 |
| 404 | 消息不存在 |

### `POST /api/messages/export`

```json
{
  "friend_id": "<uuid>",
  "format": "csv",        // 或 "json"
  "date_from": "...",     // 可选
  "date_to": "..."        // 可选
}
```

**响应**

```json
{
  "data": {
    "download_url": "/api/messages/download/<32-hex>",
    "expires_at": "...",
    "format": "csv",
    "row_count": 1234
  }
}
```

下载链接 30 分钟 TTL，**一次性**消费。

### `GET /api/messages/download/{token}`

公开（token 即凭证）。第一次返回文件，第二次 410。

---

## 表情（Emojis）

### `GET /api/emojis`

返回**当前用户**的表情列表（Redis 60s 缓存）。

### `POST /api/emojis`

```bash
curl -X POST http://localhost:8080/api/emojis \
  -b cookies.txt -H "X-CSRF-Token: <csrf>" \
  -F "file=@happy.png" -F "name=happy"
```

PNG/JPG ≤ 500KB；用户上限 100 张（超出 409）。

### `GET /api/emojis/{id}`

任意已登录用户可解析任意 emoji（用于聊天对方渲染图片）。响应不含 `user_id`。

### `DELETE /api/emojis/{id}`

仅所有者可删除；其他人 → 404（不暴露存在）。

---

## 邀请码（Invitations）

### `POST /api/invitations/generate` （admin）

```json
{ "count": 5, "expires_in_days": 7, "notes": "Q2 batch" }
```

`count` 1..=100；`expires_in_days` 1..=30。

**响应 201**

```json
{
  "data": {
    "invitations": [
      { "id": "...", "code": "INV-AbCd...", "code_prefix": "INV-AbCd", "expires_at": "..." }
    ],
    "download_url": "/api/invitations/download/<32-hex>"
  }
}
```

明文 `code` 仅本次响应返回；DB 仅存 SHA-256 hash。

### `GET /api/invitations` （admin）

```bash
curl -b admin-cookies.txt "http://localhost:8080/api/invitations?status=unused&page=1&limit=20"
```

`status` ∈ {`unused`, `used`, `revoked`, `expired`}。响应是 masked 视图，**绝不**返回 `code_hash`。

### `GET /api/invitations/stats` （admin）

```json
{ "data": { "total": 10, "used": 4, "unused": 5, "expired": 1 } }
```

### `DELETE /api/invitations/{id}` （admin）

将状态改为 `revoked`。未知 id → 404。

### `GET /api/invitations/download/{token}` （admin）

CSV 一次性下载。第二次 410。

### `POST /api/invitations/validate`

公开端点（无须登录），用于注册前提前校验邀请码。30 req/min/IP。

```json
{ "code": "INV-..." }
```

```json
{ "data": { "valid": false } }
// 或
{ "data": { "valid": true, "expires_at": "..." } }
```

不暴露任何敏感信息（不区分"不存在"vs"已用"）。

---

## WebSocket

### `GET /api/ws`

升级到 WebSocket。鉴权通过 `access_token` cookie（必须 `credentials: 'include'`）。

未登录 → 401。

#### 客户端 → 服务端

| event | 字段 |
|------|------|
| `message` | `to_user_id`, `content`, `emoji_ids?` |
| `ping` | （内置 ping/pong，30s 间隔） |

#### 服务端 → 客户端

| event | 字段 |
|------|------|
| `message_received` | `message_id`, `timestamp`（自己发的回执） |
| `message` | `message_id`, `from_user_id`, `content`, `emoji_ids`, `created_at` |
| `user_online` / `user_offline` | `user_id` |
| `error` | `code`, `message` |

详细规范见 ARCHITECTURE.md §4.7。

---

## 附录

### CSRF 双提交模式

1. 登录/注册成功 → 服务端写 `csrf_token` cookie（**非 httpOnly**，前端 JS 可读）
2. 前端 mutation 请求时读 cookie 值，附到 `X-CSRF-Token` 头
3. 服务端中间件比较 header 与 cookie 必须相等，否则 403

### Cookie 域名/路径

- `access_token`：Path=`/`，httpOnly+Secure+SameSite=Lax
- `refresh_token`：**Path=`/api/auth/refresh`**（缩小提交面），其余同上
- `csrf_token`：Path=`/`，**非 httpOnly**+Secure+SameSite=Lax

### 速率限制响应

超限时返回 `429 Too Many Requests`，body 含 `retry_after_seconds`。

---

*基于代码截至 2026-05-07 自动生成；如有变更以代码为准。*
