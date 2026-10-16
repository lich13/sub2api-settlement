# 部署

Rust stable（当前 1.99.0）、Node 24.21.0 LTS、pnpm 12.11.2。运行镜像只包含 Rust 服务与静态前端，不需要 Python。通过 Docker Compose 运行，使用已有 `sub2api_sub2api-network`，仅映射 `127.0.0.1:18082`。

创建 `.env`（参考 `.env.example`，权限 0600）和 `data/`（UID/GID 10002，权限 0700）。SQLite、WAL 及 `session.key` 属于持久数据，备份时使用 SQLite backup API，保留同一密钥。单实例运行，不挂载网络文件系统。

## 环境变量

以下默认值以直接运行 Rust 可执行文件为准；镜像覆盖值另列。应用不会自行加载 `.env`，Compose 通过 `env_file` 注入，直接运行时需在进程环境中设置。

| 变量 | 默认值与用途 |
| --- | --- |
| `SOURCE_DATABASE_URL` | 空；正式运行必须配置专用只读 PostgreSQL 连接。 |
| `PUBLIC_ORIGIN` | `https://example.invalid`；改为主站完整来源地址，不包含 `/settlement/`，用于写入 Origin 校验和 Cookie 安全属性。 |
| `SUB2API_VERIFY_URL` | `http://sub2api:8080`；服务端可访问的主站内部地址，用于身份校验和已授权删除。 |
| `DATA_DIR` | `data`；镜像为 `/data`，保存账本、密钥及备份。 |
| `SETTLEMENT_BIND` | `0.0.0.0:18082`；容器内部监听。Compose 仅向主机回环地址映射端口。 |
| `SETTLEMENT_ASSETS` | `frontend/dist`；镜像为 `/app/frontend/dist`。 |
| `APP_REVISION` | `development`；镜像来自同名构建参数，示例 `.env` 为 `local`，发布部署需设置对应提交。 |
| `SETTLEMENT_ENV` | `production`；只有精确值 `test` 启用隔离测试配置，正式环境不要使用。 |
| `SETTLEMENT_BACKGROUND` | `true`；只有 `false` 关闭定时任务，不取消启动同步或请求触发的处理，也不能替代外部写入开关。 |
| `SETTLEMENT_READ_ONLY` | `false`；只有 `true` 拒绝业务写入并禁用外部写入；认证、同步和启动迁移仍可能写 SQLite，不能理解为文件系统只读。 |
| `SETTLEMENT_EXTERNAL_WRITES` | `true`；只有 `false` 禁用外部删除和提醒；业务只读模式也会强制禁用。 |
| `RUST_LOG` | 未配置或无法解析时为 `settlement=info`；由日志组件读取，避免开启第三方库的敏感调试日志。 |
| `SETTLEMENT_SSO_PER_MINUTE` / `SETTLEMENT_WRITES_PER_MINUTE` | `30` / `120`；各为 1–100000，分别限制客户端 SSO 与用户写入。 |
| `SETTLEMENT_TRUSTED_PROXIES` | 空；最多 32 个以逗号分隔的明确 IP，不接受 CIDR。 |
| `SETTLEMENT_METRICS_ENABLED` / `SETTLEMENT_METRICS_TOKEN` | `false` / 空；精确值 `true` 启用监控；非空令牌须为 32–1024 个无空白 ASCII 字符。 |
| `SETTLEMENT_REMINDERS_ENABLED` | `false`；精确值 `true` 开启提醒，仍受测试、只读及外部写入限制。 |
| `SETTLEMENT_REMINDER_URL` / `SETTLEMENT_REMINDER_SECRET` | 空；开启提醒后须配置 HTTPS Webhook 和 32–1024 字节签名密钥。 |
| `SETTLEMENT_REMINDER_HOUR` | `9`；北京时间 0–23 点，仅在提醒启用时读取。 |

`SOURCE_FIXTURE` 无默认文件，非空时要求 `SETTLEMENT_ENV=test`；`SETTLEMENT_TEST_NOW` 只在该测试环境中接受有效的带时区时间，用于固定测试时钟。这两个变量只用于临时虚构账本，生产 `.env` 不应设置。测试环境还会绕过生产限流并默认关闭监控和外部提醒。

## 数据库与入口

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

## 运行状态与稽核

管理视图显示最近消费同步、账号核对、全量对账和未完成删除任务数。操作记录支持按类型与北京时间区间筛选、向前分页和 CSV 导出；导出超过十万条时需缩小范围。导出使用 UTF-8 BOM，文字单元格会防护电子表格公式注入，不改变账本中的原记录。只有管理员可以查看和导出。

Prometheus 入口为 `/metrics`，默认关闭。启用 `SETTLEMENT_METRICS_ENABLED=true` 后，仅允许服务实际回环连接，或携带 `Authorization: Bearer …` 且匹配 `SETTLEMENT_METRICS_TOKEN` 的采集器。令牌至少 32 位；转发头不能替代认证。默认 Nginx 配置不转发这个路径，建议仅通过受保护的内部网络采集。

指标包括 `settlement_sync_lag_seconds`、`settlement_last_successful_sync_timestamp_seconds`、`settlement_sync_healthy`、`settlement_pending_deletion_tasks` 和 `settlement_write_conflicts_total`。最后一个计数包括版本变化、旧预览和幂等冲突，重启归零；指标不包含用户、账号名称或凭据。尚未同步时成功时间为 0，结合 healthy 判断，不能把延迟为 0 当成同步成功。

## 待付款提醒（可选）

默认不发送外部提醒。设置 `SETTLEMENT_REMINDERS_ENABLED=true`、HTTPS 的 `SETTLEMENT_REMINDER_URL` 和至少 32 字节的 `SETTLEMENT_REMINDER_SECRET` 后启用通用 Webhook。URL 和密钥只放在权限 0600 的环境配置中。只读实例或禁用外部写操作的实例不会发送。

`SETTLEMENT_REMINDER_HOUR` 为北京时间的发送小时，默认 9。每天到该小时后，对存在超过 24 小时待付原单的双方按当前净额提醒，每对用户每天最多创建一条记录；净额为零不发送。请求 JSON 只包含事件 ID、时间、付款人与收款人的用户 ID、整数分金额、币种和订单数量，不发送姓名、订单明细或凭据。

接收端应以 `X-Settlement-Event` 去重，并校验 `X-Settlement-Signature: v1=<hex>`：使用共享密钥计算 `HMAC-SHA256(X-Settlement-Timestamp + "." + 原始请求体)`，同时检查时间戳新鲜度。返回任意 2xx 表示接收成功，其他状态与超时按 60、120、240、480 秒退避，最多尝试五次；不跟随重定向。失败与进程中断可能发生在接收端已收到之后，因此投递语义是至少一次，不能把提醒当成付款凭据。每次发送前重核净额摘要，已经支付或账目发生变化的队列项作废，后续日期按最新账目生成。

返回 [项目首页](../README.zh-CN.md)。
