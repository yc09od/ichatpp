# ichatpp 部署指南

> 目标：实习生跟着这份文档能在 30 分钟内完成首次自部署。
> 推荐路径是 **Coolify 自托管 PaaS**（[ARCHITECTURE.md §7](../ARCHITECTURE.md) 同步），同时保留 Nginx + docker-compose 作为备选。

## 快速决策

| 路径 | 适用场景 | 学习成本 | 时间 |
|------|---------|----------|------|
| **A. Coolify**（推荐） | 自有 VPS + 想要"git push 即部署"+ 自动 SSL | 低 | ~30 min 首次 |
| B. Nginx + docker-compose | 已有手写 Nginx 配置 / 不想引入额外平台 | 中 | ~1h 首次 |

下文先讲 A，B 在最后单独成章。

---

# 路径 A：Coolify

## 0. 前置准备

- 一台 Linux VPS（≥ 2 GB RAM、≥ 20 GB 磁盘；Ubuntu 22.04 或 Debian 12 推荐）
- 一个域名（例：`chat.example.com`）— DNS A 记录指向 VPS IP
- 已开放端口 80 / 443
- 在 VPS 上安装 Coolify（一行命令，详见 [coolify.io/docs](https://coolify.io/docs)）
- 已 fork 或 clone 本仓库到一个可访问的 git 远程（GitHub / GitLab / 自建 Gitea）

## 1. Coolify 内创建项目骨架

登录 Coolify Dashboard：

1. **+ New Project** → 名字 `ichatpp`
2. 进入项目，**+ New Resource** 添加 3 个数据 service：
   - **Postgres**（模板，版本 14）→ 命名 `ichatpp-postgres`
   - **Redis**（模板，版本 6，**勾选 password**）→ 命名 `ichatpp-redis`
   - **MinIO** → 命名 `ichatpp-minio`（或跳过用外部 S3）

每个 service 创建后，Coolify 会显示**内部连接字符串**，记下来：

```
Postgres: postgresql://<user>:<pwd>@ichatpp-postgres:5432/<db>
Redis:    redis://:<pwd>@ichatpp-redis:6379
MinIO:    http://ichatpp-minio:9000   (admin via :9001)
```

> **持久化卷**：3 个 service 都已默认开启卷挂载，数据在容器重建后保留。

## 2. 添加 Backend Application

**+ New Resource** → **Application** → **Public Repository** 或私有 git：

- Repository: `https://github.com/<you>/ichatpp.git`
- Branch: `main`
- Build Pack: **Dockerfile**
- Dockerfile Location: `backend/Dockerfile.backend`（详见 TODO [53]）
- Base Directory: `/`（或 `backend/`，与 Dockerfile 中的 path 配合）
- Port: `8080`

### 2.1 环境变量（Backend）

在该 application 的 **Environment Variables** 标签页填入：

```
DATABASE_URL=postgresql://<user>:<pwd>@ichatpp-postgres:5432/<db>
REDIS_URL=redis://:<pwd>@ichatpp-redis:6379

S3_ENDPOINT=http://ichatpp-minio:9000
S3_ACCESS_KEY=<minio_admin_user>
S3_SECRET_KEY=<minio_admin_pwd>
S3_REGION=us-east-1
S3_BUCKET_AVATARS=avatars
S3_BUCKET_EMOJIS=emojis
S3_BUCKET_EXPORTS=exports

JWT_PRIVATE_KEY_PATH=/run/secrets/jwt-private.pem
JWT_PUBLIC_KEY_PATH=/run/secrets/jwt-public.pem
JWT_ACCESS_TTL_SECONDS=3600
JWT_REFRESH_TTL_SECONDS=604800

BIND_ADDR=0.0.0.0:8080
RUST_LOG=info,ichatpp=info

FRONTEND_ORIGIN=https://chat.example.com
COOKIE_DOMAIN=chat.example.com
COOKIE_SECURE=true
```

### 2.2 持久卷（用于 JWT 密钥）

在 **Persistent Storage** 标签页，添加：

- Mount Path: `/run/secrets`
- Volume Name: `ichatpp-secrets`

保存后，**先不启动**应用。

## 3. 添加 Frontend Application

同 backend，但：
- Dockerfile Location: `frontend/Dockerfile.frontend`
- Port: `3000`

环境变量：

```
NEXT_PUBLIC_API_BASE_URL=https://chat.example.com
NEXT_PUBLIC_WS_URL=wss://chat.example.com/api/ws
NODE_ENV=production
```

## 4. 配置域名（单域名同源路径分流）

**关键设计：** 前后端共享一个域名，由 Coolify 的反向代理（Caddy/Traefik）按路径分流，消除 CORS。

在 Coolify 的 backend application 设置 → **Domains**：

```
https://chat.example.com/api
```

在 frontend application 设置 → **Domains**：

```
https://chat.example.com
```

> Coolify 会自动按路径前缀路由：`/api/*` → backend，其余 → frontend。WebSocket 升级（`/api/ws`）由 Caddy 透传。

> 如果 Coolify UI 没有"路径前缀"选项，则使用一个外部 Caddy/Traefik 容器作为前置反代，或在每个 application 上声明独立子域（`chat.example.com` 与 `api.example.com`），后者需要把 `FRONTEND_ORIGIN` 改成对应值并打开 CORS。

## 5. 生成 JWT 密钥（首次部署一次）

在 Coolify 的 backend application → **Terminal** 标签页打开 shell（或 Coolify 主机上用 docker exec），执行：

```bash
mkdir -p /run/secrets
openssl genpkey -algorithm RSA -out /run/secrets/jwt-private.pem -pkeyopt rsa_keygen_bits:2048
openssl rsa -in /run/secrets/jwt-private.pem -pubout -out /run/secrets/jwt-public.pem
chmod 600 /run/secrets/jwt-private.pem
```

**密钥仅生成一次，写在持久卷里**；后续容器重建会自动复用。

> ⚠️ **同时备份**：把 `/run/secrets/` 备份到安全离线位置。私钥丢失 = 全用户被迫重新登录；私钥泄漏 = 必须立即重新生成 + 全部 token 失效。

## 6. 初始化 MinIO 桶

进 `https://<minio-console-url>:9001` 用 root 凭证登录，创建桶：
- `avatars`（设置 anonymous policy: `download`）
- `emojis`（设置 anonymous policy: `download`）
- `exports`（保持私有）

或在 Coolify 主机上等价命令：

```bash
docker exec -it <minio-container> sh -c '
  mc alias set local http://localhost:9000 $MINIO_ROOT_USER $MINIO_ROOT_PASSWORD &&
  mc mb -p local/avatars local/emojis local/exports &&
  mc anonymous set download local/avatars &&
  mc anonymous set download local/emojis
'
```

## 7. 启动 + 创建管理员

1. Coolify backend application → **Deploy**。Coolify 拉 git → docker build → 启动容器
2. **数据库迁移在 backend 启动时自动跑**（[`main.rs`](../backend/src/main.rs) 里 `sqlx::migrate!("./migrations").run(&db)`，迁移 SQL 在编译期嵌入二进制，runner 镜像不需要 sqlx-cli）。
3. 启动成功后，在 Coolify backend application 的 Terminal 里创建初始管理员：

```bash
/app/seed_admin --email admin@your.domain --password 'Admin12345!'
```

4. 启动 frontend application → Deploy

## 8. 验证

```bash
# 健康检查
curl -i https://chat.example.com/api/health
# → HTTP/2 200, {"status":"ok"}

# 访问前端
open https://chat.example.com
```

走一遍：登录管理员 → `/admin/invitations` 生成邀请码 → 退出 → 用邀请码注册新用户 → 加好友 → 发消息。

---

## 后续运维

### 自动重新部署
Coolify 支持 git webhook：仓库 push 触发自动 build + redeploy。在 application 设置 → **Webhooks** 复制 URL 粘到 GitHub Repository Settings。

### 备份
- **Postgres**：Coolify Resource → **Backups** → 启用每日定时（保留 7 天）。手动备份：`pg_dump $DATABASE_URL > backup.sql`
- **MinIO**：复制 `miniodata` 卷或开桶级 versioning。备用：`mc mirror local/avatars /backup/avatars/`
- **JWT 密钥**：`/run/secrets/` 单独离线备份（安全位置 + 加密）
- **Redis**：通常无需备份（仅缓存 + 短期会话状态）

### 升级 / 重新部署
- 代码变更：git push → Coolify 自动重新部署
- 配置变更：Coolify UI 改完 → Restart application
- Coolify 自身升级：参考 Coolify 官方文档

### 监控
- Coolify 内置容器健康状态 + 资源使用图表
- 进阶：接 Prometheus + Grafana（backend 暴露 `/metrics` 是后续优化点）

### 扩容
- 单机加内存/CPU 通常足够 MVP 至中等用户量
- 需要多实例：在 Coolify 复制 backend application（共享同一组数据 service）。WebSocket 在线状态通过 Redis Pub/Sub 跨实例广播（[ARCHITECTURE.md §4.7](../ARCHITECTURE.md)），不需要 sticky session

---

## 故障排查

### 后端启动失败：JWT key not readable
检查持久卷已挂载到 `/run/secrets/`，密钥文件存在且权限可读。

### 前端登录后跳回 /login
通常是 cookie 不生效。检查：
1. `COOKIE_SECURE=true` 必须配合 HTTPS（确认证书有效）
2. `COOKIE_DOMAIN` 与实际域名一致
3. 浏览器开发者工具 → Application → Cookies 看是否真有 access_token / csrf_token

### WebSocket 升级失败（101 → 502）
确认 Coolify Caddy/Traefik 配置允许 `Upgrade: websocket` 透传（默认应允许）。如果用了独立反代（Cloudflare 等），开启 WebSocket 选项。

### MinIO 上传成功但前端图片打不开
匿名 download 策略未设置 → `mc anonymous set download local/<bucket>`。

### 迁移失败：role does not exist
`DATABASE_URL` 中的用户与 Coolify 创建 Postgres 时配置的不一致。

---

# 路径 B：Nginx + docker-compose（备选）

适合：已有自管 Nginx 经验、偏好显式配置、不想引入 Coolify。

## 1. 服务器准备

- Ubuntu 22.04+，已安装 docker + docker-compose-plugin
- 防火墙开 80/443
- DNS：`chat.example.com` 指向服务器

## 2. 拉代码 + 写 .env

```bash
git clone https://github.com/<you>/ichatpp.git
cd ichatpp
cp .env.example .env
# 编辑 .env，把开发凭证改为生产值
```

## 3. 生成 JWT 密钥

```bash
mkdir -p backend/keys
openssl genpkey -algorithm RSA -out backend/keys/jwt-private.pem -pkeyopt rsa_keygen_bits:2048
openssl rsa -in backend/keys/jwt-private.pem -pubout -out backend/keys/jwt-public.pem
chmod 600 backend/keys/jwt-private.pem
```

## 4. 构建生产镜像

```bash
docker build -t ichatpp-backend:latest -f backend/Dockerfile.backend backend
docker build -t ichatpp-frontend:latest -f frontend/Dockerfile.frontend frontend
```

## 5. 起栈

写一个 `docker-compose.prod.yml`（基于已有 `docker-compose.yml` 增加 backend / frontend 容器；略），然后：

```bash
docker compose -f docker-compose.prod.yml up -d
```

## 6. Nginx 反代 + Let's Encrypt

```nginx
# /etc/nginx/sites-available/ichatpp
server {
    listen 443 ssl http2;
    server_name chat.example.com;

    ssl_certificate     /etc/letsencrypt/live/chat.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/chat.example.com/privkey.pem;

    # WebSocket
    location /api/ws {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_read_timeout 600s;
    }

    # API
    location /api/ {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }

    # 前端
    location / {
        proxy_pass http://127.0.0.1:3000;
        proxy_set_header Host $host;
    }
}

server {
    listen 80;
    server_name chat.example.com;
    return 301 https://$host$request_uri;
}
```

```bash
sudo ln -s /etc/nginx/sites-available/ichatpp /etc/nginx/sites-enabled/
sudo certbot --nginx -d chat.example.com
sudo nginx -t && sudo systemctl reload nginx
```

## 7. 建管理员

迁移在 backend 启动时自动跑（见 [`main.rs`](../backend/src/main.rs)），所以这里只需要建管理员：

```bash
docker compose -f docker-compose.prod.yml exec backend /app/seed_admin --email a@b.c --password Admin12345
```

## 8. 验证 同 A 路径 §8

---

## 环境变量速查（两路径通用）

| Key | 必填 | 默认 | 说明 |
|-----|------|------|------|
| `DATABASE_URL` | ✅ | — | postgres://user:pwd@host:port/db |
| `REDIS_URL` | ✅ | — | redis://:pwd@host:port |
| `S3_ENDPOINT` | ✅ | — | http://host:9000（MinIO）或 https://s3.amazonaws.com |
| `S3_ACCESS_KEY` | ✅ | — | |
| `S3_SECRET_KEY` | ✅ | — | |
| `S3_REGION` | — | `us-east-1` | |
| `S3_BUCKET_AVATARS` | — | `avatars` | |
| `S3_BUCKET_EMOJIS` | — | `emojis` | |
| `S3_BUCKET_EXPORTS` | — | `exports` | |
| `JWT_PRIVATE_KEY_PATH` | ✅ | — | 必须可读 |
| `JWT_PUBLIC_KEY_PATH` | ✅ | — | |
| `JWT_ACCESS_TTL_SECONDS` | — | `3600` | |
| `JWT_REFRESH_TTL_SECONDS` | — | `604800` | |
| `BIND_ADDR` | — | `0.0.0.0:8080` | |
| `RUST_LOG` | — | `info` | 生产建议 `info`，调试用 `info,ichatpp=debug` |
| `FRONTEND_ORIGIN` | ✅ | — | CORS allow-list；单域名同源时仍要填实际值 |
| `COOKIE_DOMAIN` | ✅ | `localhost` | 必须与生产域名一致 |
| `COOKIE_SECURE` | ✅ | `false` | **生产必须 true（要求 HTTPS）** |
| `NEXT_PUBLIC_API_BASE_URL` | ✅（前端） | — | 单域名同源时填生产域名 |
| `NEXT_PUBLIC_WS_URL` | ✅（前端） | — | wss://… |

---

*基于 ARCHITECTURE.md §7 与现有 docker-compose 配置生成；遇问题先查上文"故障排查"。*
