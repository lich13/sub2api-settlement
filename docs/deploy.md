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

## 消费同步

启动时检查源库字段、类型和读取权限，并进行一次完整核对。之后每分钟读取水位线之后的新 ID，同时复核最近 10 分钟的记录；新增但回填为历史时间的请求仍会采集。账号状态保持独立的 5 秒核对。

每 24 小时按账号与 UTC 日期对账，比较笔数、原始金额总和及包含用户、请求标识和 Token 字段的指纹，仅重新读取变化的范围。指纹只决定是否重新扫描，不作为金额或结算摘要。清理任务变化或已知母子归属变更会触发完整核对。对账仍需在源库扫描指纹，不能替代源库索引与容量规划。

水位线、指纹和消费副本在同一次发布事务中更新。同步失败保留上一批有效结果，并显示同步错误；重新启动会完整核对。较老记录的原地修改最迟在每日对账发现，正式预览与结算始终单独复核所选账期。历史消费副本不会随源日志清理而删除。

## 访问与发布安全

SSO 默认每个客户端 IP 每分钟 30 次；业务写入默认每个已验证用户每分钟 120 次，超限返回 `429` 和 `Retry-After: 60`。可分别用 `SETTLEMENT_SSO_PER_MINUTE`、`SETTLEMENT_WRITES_PER_MINUTE` 调整。内存限流表最多保存 10000 个活跃身份，满额时拒绝新的身份请求，过期后释放。

只有回环连接或 `SETTLEMENT_TRUSTED_PROXIES` 中明确列出的代理 IP 可以提供 `X-Real-IP`，不信任任意客户端提交的转发头。Docker 桥接部署时应填入实际 Nginx 连接来源 IP，并确保代理覆盖该头；不配置则按连接来源限流。多个用户共用同一未识别代理时会共享 SSO 限额。

Nginx 配置已关闭结算路径访问日志并设置 `Referrer-Policy: no-referrer`，避免 SSO 查询参数被记录或带往其他页面。Compose 使用只读根文件系统、移除全部 capabilities、禁止提权和运行时健康检查；仅数据卷与临时目录可写。

基础镜像固定 digest，CI 检查 Rust 与前端依赖，发布时生成 SPDX SBOM，并用 GitHub Actions OIDC 对同一个已测试镜像 digest 签名。源码、静态前端、Linux amd64 镜像包、签名与摘要文件随 Release 提供。依赖许可及唯一的未激活依赖例外见 [SECURITY.md](../SECURITY.md)。

返回 [项目首页](../README.zh-CN.md)。
