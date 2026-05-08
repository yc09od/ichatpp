# ichatpp

> 一个开源的实时通讯网页应用，为朋友社交提供文字消息和聊天记录功能。

## 项目简介

ichatpp 是一个类似微信的网页通讯应用，专为朋友社交场景设计。用户必须凭管理员发放的邀请码注册账号，注册后可添加好友、进行实时文字聊天，应用会自动保存所有对话记录。应用还支持使用静态图片（PNG/JPG）作为自定义表情，增强社交互动体验。

这是一个开源项目，旨在探索现代实时通讯技术在Web端的实现，使用Next.js前端和Rust后端，为开发者提供完整的技术参考。

## 核心功能

- **邀请码注册**：所有新用户必须凭管理员发放的邀请码注册（一次性、7 天过期），保证社区可控
- **管理员系统**：管理员可生成、查看、撤销邀请码，并支持批量导出
- **账号系统**：邮箱密码登录、用户档案管理、唯一账号码自动分配
- **好友管理**：通过账号码添加好友、好友请求处理、好友列表
- **实时聊天**：WebSocket 实时文字消息传输，支持多用户在线状态
- **消息记录**：所有对话自动保存，支持查看历史、关键词搜索、导出聊天记录
- **自定义表情**：支持上传 PNG/JPG 图片作为表情，在聊天中快速使用

## 技术栈

- **前端**：Next.js（React 框架）、TypeScript、WebSocket、Tailwind CSS
- **后端**：Rust（Actix-web 框架）、PostgreSQL、Redis
- **实时通讯**：WebSocket（前后端）
- **存储**：PostgreSQL（数据）、MinIO/S3（图片存储）
- **部署**：Docker 多阶段镜像、Docker Compose（开发）、Coolify 自托管 PaaS（生产首选）

## 快速开始

### 前置要求
- Node.js 18+（前端开发）
- Rust 1.70+（后端开发）
- PostgreSQL 12+ / Docker
- Redis 6+（可选，用于会话缓存）

### 安装与运行

**前端：**
```bash
cd frontend
npm install
npm run dev
# 访问 http://localhost:3000
```

**后端：**
```bash
cd backend
cargo build
cargo run
# API 服务运行在 http://localhost:8080
```

### 使用 Docker Compose 快速启动

```bash
docker-compose up
```

## 项目状态

📋 **规划中** - 架构设计和开发计划阶段

## License

MIT License - 开源自由使用

---

*最后更新：2026-05-07*
