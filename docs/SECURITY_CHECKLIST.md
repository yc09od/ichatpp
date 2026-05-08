# ichatpp 上线前安全自检报告

> 执行日期：2026-05-07
> 检查范围：源代码 + 配置 + 数据库 schema
> 验证方式：代码 grep + 集成测试断言（[47] 覆盖率 81.55%）

## 总览

| # | 检查项 | 结果 | 证据 |
|---|--------|------|------|
| 1 | 所有 token 仅在 httpOnly cookie | ✅ | [详情](#1-所有-token-仅在-httponly-cookie) |
| 2 | 数据库无明文邀请码 | ✅ | [详情](#2-数据库无明文邀请码) |
| 3 | 所有 mutation 经 CSRF 中间件 | ✅ | [详情](#3-所有-mutation-经-csrf-中间件) |
| 4 | bcrypt cost ≥ 12 | ✅ | [详情](#4-bcrypt-cost--12) |
| 5 | JWT 使用 RS256 | ✅ | [详情](#5-jwt-使用-rs256) |
| 6 | 限流生效 | ✅ | [详情](#6-限流生效) |
| 7 | 错误响应不暴露内部细节 | ✅ | [详情](#7-错误响应不暴露内部细节) |

**结论：7/7 全部通过**，可以上线。下方按项给出代码证据 + 测试覆盖。

---

## 1. 所有 token 仅在 httpOnly cookie

**实现位置：** [`backend/src/auth/cookies.rs`](../backend/src/auth/cookies.rs) — `set_auth_cookies` 是唯一写 token cookie 的入口。

**验证结果：**
- access_token cookie：`HttpOnly=true`、`Secure=<COOKIE_SECURE>`、`SameSite=Lax`、`Path=/`
- refresh_token cookie：同上 + `Path=/api/auth/refresh`（缩小提交面）
- csrf_token cookie：`HttpOnly=false`（前端 JS 必须能读）+ `Secure` + `SameSite=Lax`

**响应体不返 token：**
- `RegisterResponse` 仅 `{ user_id, account_code }`
- `LoginResponse` 同上
- `RefreshResponse` 仅 `{ user_id }`
- `LogoutResponse` 仅 `{ ok }`

**测试断言（pin 覆盖）：**
- [`tests/auth.rs::register_emits_three_cookies_and_persists_user`](../backend/tests/auth.rs)：响应体扫描 `eyJ`（JWT 前缀）必须**不存在**
- 同文件确认所有 3 个 cookie 都被 `Set-Cookie` 写入

**前端：**
- [`frontend/lib/api.ts`](../frontend/lib/api.ts) 全部走 `fetch(..., credentials: 'include')`
- 全文未 grep 到 `localStorage.setItem('token'`/`sessionStorage.setItem('token'` 等模式

---

## 2. 数据库无明文邀请码

**Schema：** [`backend/migrations/20260507180623_create_initial_schema.up.sql`](../backend/migrations/) 中 `invitations` 表的列：

```sql
code_hash CHAR(64) NOT NULL UNIQUE,    -- SHA-256(plaintext) hex (64 chars)
code_prefix VARCHAR(8),                -- 明文前 8 位，仅用于 admin 列表识别
```

**没有 `code` 或 `code_plaintext` 列。**

**生成路径（[`backend/src/handlers/invitations.rs`](../backend/src/handlers/invitations.rs)）：**
1. `generate_code()` — OsRng 产 base62 20 位
2. `hash_code(plain) → SHA-256 → hex`
3. `insert_invitation(...)` 只写 `code_hash` 与 `code_prefix`，**绝不写明文**
4. 明文仅在 `POST /api/invitations/generate` 响应中返回**一次**，并通过 Redis 临时键（10 分钟 TTL）支持一次性 CSV 下载

**测试断言：**
- [`tests/invitations.rs::list_returns_masked_view_without_hash`](../backend/tests/invitations.rs)：list 响应中**禁止**出现 `code_hash` 字段
- [`tests/invitations.rs::download_returns_csv_then_410_on_replay`](../backend/tests/invitations.rs)：第二次访问下载 token → 410

**Admin 列表 masked 视图：** 仅返回 `code_prefix`（如 `INV-AbCd`），不可推回明文。

---

## 3. 所有 mutation 经 CSRF 中间件

**实现位置：** [`backend/src/middleware/csrf.rs`](../backend/src/middleware/csrf.rs) — `CsrfProtection` middleware，在 [`backend/src/main.rs`](../backend/src/main.rs) 全局 `App::wrap()`。

**双提交模式：**
- 登录成功下发非 httpOnly `csrf_token` cookie
- 前端 mutation 时把 cookie 值附到 `X-CSRF-Token` 头
- 中间件比较 header 与 cookie 必须相等 → 否则 403

**豁免清单（仅这两个）：**
- `POST /api/auth/login`（无 cookie 时无法签 CSRF）
- `POST /api/auth/register`（同上）

**测试覆盖：** [`backend/src/middleware/csrf.rs`](../backend/src/middleware/csrf.rs) 的内部测试模块 4 个测试，覆盖：
- 缺失头 → 403
- header ≠ cookie → 403
- header == cookie → pass
- GET/HEAD 不校验

集成测试 ([`tests/auth.rs`](../backend/tests/auth.rs) 等) 通过 mint 直签 cookie 绕开 CSRF（仅测试中），生产路径未 bypass。

---

## 4. bcrypt cost ≥ 12

**证据：** [`backend/src/handlers/auth.rs:84`](../backend/src/handlers/auth.rs)

```rust
const BCRYPT_COST: u32 = DEFAULT_COST;  // bcrypt 0.15 → 12
```

bcrypt crate `0.15.x` 的 `DEFAULT_COST` 常量值为 **12**（见 [docs.rs/bcrypt](https://docs.rs/bcrypt/)）。

**使用点：**
- `register_handler` → `bcrypt_hash(password, BCRYPT_COST)`
- `dummy_password_hash()` → 同 cost，保持 timing 不可区分

**注：** 测试 fixture（`seed_user`）显式用 cost 4 加速 — 仅 `tests/common/mod.rs` 中，生产代码不使用。

---

## 5. JWT 使用 RS256

**证据：** [`backend/src/auth/jwt.rs`](../backend/src/auth/jwt.rs)

```rust
use jsonwebtoken::Algorithm;
encode(&Header::new(Algorithm::RS256), ...)              // 签名
let validation = Validation::new(Algorithm::RS256);      // 验签
```

**密钥：**
- `EncodingKey::from_rsa_pem(...)` — 仅签发使用，私钥
- `DecodingKey::from_rsa_pem(...)` — 验签使用，公钥
- 通过 [`Keys::from_config`](../backend/src/auth/jwt.rs) 从 `JWT_PRIVATE_KEY_PATH` / `JWT_PUBLIC_KEY_PATH` 读取
- 启动时**eager**加载（缺失或损坏直接 panic 不让服务起来）

**Token 类型隔离：** `typ` 自定义 claim，refresh ↔ access 不可互换（`verify_token(token, TokenType::Access)` 拒绝 refresh token）。

**测试覆盖：** [`backend/src/auth/jwt.rs`](../backend/src/auth/jwt.rs) `mod tests` 9 个用例覆盖：签发/验证、过期、错误签名、typ 替换攻击。

---

## 6. 限流生效

**实现：** actix-governor，在 [`backend/src/middleware/mod.rs`](../backend/src/middleware/mod.rs) 配置三档：

| 范围 | 限制 | 配置常量 |
|------|------|----------|
| 全局 | 100 req/min/IP | `global_governor_config()` |
| `/api/auth/*` | 10 req/min/IP | `auth_governor_config()` |
| `/api/invitations/validate` | 30 req/min/IP | `invitation_validate_governor_config()` |

**应用：** 在 [`main.rs`](../backend/src/main.rs) `.wrap(Governor::new(&...))` 在对应 scope 上。

**额外的应用层限流：**
- 登录失败 5 次 → email 锁定 15 分钟（Redis INCR + TTL，[`auth.rs:495`](../backend/src/handlers/auth.rs)）
- 表情上传 100 张/用户上限（[`emojis.rs:50`](../backend/src/handlers/emojis.rs)）

**测试覆盖：** [`tests/auth.rs::login_locks_after_five_failures`](../backend/tests/auth.rs) 验证 email 锁定。

> **注意：** Coolify/Cloudflare 这种反代环境下，governor 看到的 IP 可能是反代 IP。生产部署需配置 `X-Forwarded-For`/`X-Real-IP` 信任，或在边缘做 IP 限流。已在 DEPLOYMENT.md 中提示。

---

## 7. 错误响应不暴露内部细节

**实现：** [`backend/src/errors/mod.rs`](../backend/src/errors/mod.rs) — `AppError` 枚举 + `ResponseError` 实现，统一响应：

```json
{ "error": { "code": "...", "message": "...", "details": null } }
```

**关键脱敏：**
- `Internal(anyhow::Error)` → 仅返 generic `"内部错误"` + HTTP 500；anyhow 链式信息只进 log，**不进响应**
- 登录失败统一返回 `"邮箱或密码错误"`（[`auth.rs:139`](../backend/src/handlers/auth.rs)）— 不暴露 email 是否存在
- 邀请码验证失败统一返回 `{ valid: false }` —不区分"不存在"/"已用"/"已过期"
- 不可见用户对非好友 → 404，与"用户不存在"语义合并
- Refresh 失败统一返回 `"invalid or expired refresh token"` — 不区分缺失/签名错误/已撤销

**测试断言（pin 覆盖）：**
- [`tests/auth.rs::login_returns_401_for_unknown_email`](../backend/tests/auth.rs)
- [`tests/auth.rs::login_returns_401_for_wrong_password`](../backend/tests/auth.rs) — 两条都返回同一文案/HTTP code
- [`tests/invitations.rs::validate_returns_invalid_for_unknown_code`](../backend/tests/invitations.rs)

**敏感字段过滤（response DTO 层）：**
- `users.password_hash` 不在任何 `SelfUser` / `PublicUser` DTO 中
- `invitations.code_hash` 不在任何列表/详情 DTO 中（[`invitations.rs`](../backend/src/handlers/invitations.rs) 的 `mod tests` 有 pin 测试）
- `emojis` by-id 端点不返 `user_id`（[`emojis.rs::emoji_view_omits_user_id`](../backend/src/handlers/emojis.rs)）
- 好友请求 `from_user` 仅含公开字段（test pin: [`pending_request_omits_sender_private_fields`](../backend/src/handlers/friends.rs)）

---

## 边角验证（额外做的，不在 7 项内）

### 邀请码下载 token 一次性消费
[`tests/invitations.rs::download_returns_csv_then_410_on_replay`](../backend/tests/invitations.rs)：Redis `GETDEL` 原子操作，第二次访问 410 Gone。

### 消息搜索的安全边界
[`tests/messages.rs::search_excludes_other_users_messages`](../backend/tests/messages.rs)：Alice 不能搜到 Bob↔Charlie 的对话内容。SQL 强制 `(from_user_id = me OR to_user_id = me)`，无法绕开。

### 软删除消息内容脱敏
[`tests/messages.rs::history_blanks_deleted_messages`](../backend/tests/messages.rs)：`is_deleted=true` 的消息历史中 `content` 被替换为占位文本，原文仅 DB 保留（用于审计/恢复，应用层不暴露）。

### Admin 自降级保护
[`tests/users.rs::update_role_self_demotion_rejected`](../backend/tests/users.rs)：admin 不能把自己降为 user，避免锁死管理员入口。

---

## 上线前必做的非自动化项

代码已通过自检，但下面这些是**部署期人工动作**，不能从代码层面保证：

- [ ] **HTTPS 全程**：`COOKIE_SECURE=true` 必须配合有效证书
- [ ] **JWT 私钥保管**：[`/run/secrets/jwt-private.pem`](../backend/keys/) 文件权限 `600`，离线备份到加密媒介
- [ ] **数据库密码强度**：`DATABASE_URL` 中的密码不应是默认值（生产必须改）
- [ ] **MinIO root 密码**：默认 `minioadmin/minioadmin` 必须改
- [ ] **`FRONTEND_ORIGIN` 精确值**：CORS allow-list 必须是生产域名，禁止 `*`
- [ ] **`.env` 不进 git**：`.gitignore` 已排除，验证 `git ls-files | grep -E '^\.env$'` 为空
- [ ] **反代信任 IP 头**：在 Nginx/Caddy 设置 `X-Forwarded-For`，让 governor 限流命中真实客户端 IP
- [ ] **Postgres 备份**：开启 Coolify 定时备份或 cron `pg_dump`
- [ ] **首次管理员后撤销 seed**：`seed_admin` 是一次性脚本，正常情况下不需要再跑；如确认后可考虑从镜像移除

---

## 检查方法学

| 项目 | 自动化方式 |
|------|-----------|
| 1, 7 | 集成测试 pin（响应体不含敏感字段） |
| 2 | Schema 检查 + 测试 pin |
| 3 | Middleware 单元测试 |
| 4, 5 | 源代码常量 grep |
| 6 | governor 配置常量 + 应用层锁定测试 |

如修改了任一相关代码，**重跑 `cargo test`**（应保持 327 通过 + 81% 覆盖率），并按对应章节重新 grep 验证。

---

*生成于 2026-05-07，对应 commit 状态见 git log。后续修改安全相关代码时，请同步更新本文件。*
