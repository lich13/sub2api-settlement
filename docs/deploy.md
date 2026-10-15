# 部署

Rust stable（当前 1.99.0）、Node 24.21.0 LTS、pnpm 12.11.2。运行镜像只包含 Rust 服务与静态前端，不需要 Python。通过 Docker Compose 运行，使用已有 `sub2api_sub2api-network`，仅映射 `127.0.0.1:18082`。

创建 `.env`（参考 `.env.example`，权限 0600）和 `data/`（UID/GID 10002，权限 0700）。SQLite、WAL 及 `session.key` 属于持久数据，备份时使用 SQLite backup API，保留同一密钥。单实例运行，不挂载网络文件系统。

`SOURCE_DATABASE_URL` 使用专用只读角色，仅授予以下字段 SELECT：

- `accounts`: id, name, platform, status, parent_account_id, created_at, deleted_at
- `users`: id, username, role, status, deleted_at
- `usage_logs`: id, account_id, user_id, total_cost, created_at, request_id, upstream_request_id, model, requested_model, upstream_model, upstream_response_model, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens
- `usage_cleanup_tasks`: id, filters, deleted_rows, finished_at

运行 `docker compose up -d --build`。在主站 TLS server 中加载 `deploy/settlement.location.conf`，检查并 reload Nginx。通过 Sub2API 管理接口向 `custom_menu_items` 合并添加：

```json
{"id":"settlement-center","label":"结算中心","url":"https://example.invalid/settlement/sso/start","visibility":"user","sort_order":100,"hide_open_button":true,"icon_svg":""}
```

`icon_svg` 使用 `frontend/src/assets/settlement.svg` 的完整内容，与页面页头共用同一原稿。更新时读取当前菜单并仅合并该菜单项的图标。

首跳 Token 换取加密会话，重定向会清除 URL 凭据。所有接口通过 `/api/v1/auth/me` 重新校验上游身份，写请求校验 Origin、CSRF、权限、版本及幂等编号。出单后删除复用该管理员会话调用官方管理接口，凭据不进入删除任务、审计或响应；会话失效须重新登录后重试。源库从不写入；连接不选择凭据、邮箱、API Key 内容或请求正文。


## Sub2API 兼容范围

当前读取契约对照 Sub2API [v0.2.16](https://github.com/Wei-Shaw/sub2api/tree/v0.2.16) 的表结构与官方管理员接口；源库需要包含[迁移 232](https://github.com/Wei-Shaw/sub2api/blob/v0.2.16/backend/migrations/232_add_usage_log_upstream_request_id.sql) 引入的 `upstream_request_id`。测试以隔离 PostgreSQL 表和字段级只读角色验证查询，不等同于对所有 Sub2API 分支的端到端兼容承诺。其他版本或分支应先核对上述字段及类型，再在账本副本上验证。

| 字段类型 | 要求 |
| --- | --- |
| 主键、账号与用户关联 | PostgreSQL `bigint` |
| 金额 | 精确的 `numeric`，不能替换为浮点数 |
| 时间 | 带时区的 `timestamp with time zone` |
| Token 数 | `integer` 或 `bigint` |
| 名称、状态、模型与请求标识 | `text` 或 `character varying` |
| 清理任务过滤条件 | `jsonb` |

读取字段不包含邮箱、凭据、API Key 或请求正文。只读角色应仅有上表所列字段的 SELECT 权限；不得使用数据库所有者或超级用户。账号删除通过当前管理员会话调用官方 HTTP API，不使用源库写权限。

健康检查：`GET /settlement/healthz`。核对 `engine: rust`、版本和提交修订。主站登录模式由 Sub2API 控制，本项目不会自动开放注册或修改登录策略。

返回 [项目首页](../README.zh-CN.md)。
