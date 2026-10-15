# Sub2API 结算中心

独立的账号成本、固定费用与线下付款账本。Sub2API 同域 iframe 免二次登录；普通用户只看自己的分摊和账单，付款人确认转账后订单自动结清，管理员管理周期与成本。

## 结算规则

- 原始消费 `usage_logs.total_cost` 决定占比；人民币真实成本按整数分保存，界面金额统一两位小数。原价计算不提前舍入，尾差采用最大余数法、用户 ID 升序打破并列。
- 成本分为准确与模糊，旧成本默认准确。模糊成本须确认准确或导入上游账单后出单。未结算成本可修改，旧预览随版本变化失效；结算后冻结快照。自己的份额自行承担，零消费由垫付人承担。
- 周期为北京时间输入的 `[start,end)`。同账号不重叠；影子账号归并母账号。免费资源不出单。
- 周期起点支持精确时间、按时间估算和从最早已采集使用开始。时间估算精确到北京时间分钟，从所选分钟 00 秒起计入消费，预览中确认起点后才能出单。旧日期估算及旧接口继续按当天 00:00 至次日 00:00 处理；最早使用起点记录来源用量 ID。
- 免费账号在账号成本默认列表隐藏；管理员通过“免费资源”筛选“升级付费”，保存时开始计费，升级前用量不计入，历史账目保留。
- 已删除的历史账号从运营筛选、账号成本和周期列表隐藏，底层历史账目与用量记录保留。账号状态独立轻量核对，页面可见时每 5 秒刷新，不等待整批消费同步；核对失败保留最近状态并提示。
- 账号成本页可单独结算或结算并续费；续费周期从上一周期结束时间连续开始，成本、垫付人与账单独立计算。
- 可勾选“出单后删除账号”，与续期互斥。出单成功后调用官方删除接口，包含关联影子账号；删除失败不撤销账单，可在管理视图单独重试或取消任务。结果不确定或重启后先核验源状态，避免重复操作。
- 同两人之间的全部待付款订单自动对冲，包含跨账号、周期、固定费用和已有合并单；每张原单只计一次，不进行多人债务转移。净付款人确认转账后双方原单一次结清，零净额由任一当事人确认对冲。收款页只读显示净应收。历史待处理订单仍由管理员单独处理。
- 打开结算弹窗即自动核算，修改截止时间、确认起点或账单后自动刷新；预览过期重新核算，截止时间保持不变。正式出单仍需点击“确认结算”。
- 付款确认会重新核对双方完整订单集合；订单新增、作废、合并、拆分或版本变化时须刷新后重新确认。已结清的对冲记录保存双向原额、抵扣金额和实际转账额，原始金额与分摊快照不改写。
- 未报已付的整次结算可作废重算；已报已付或结清的账目锁定。无需外接支付服务，不修改 Sub2API 余额。

- 固定费用按手选参与人均摊或自定义比例分摊，比例最多两位小数且合计 100.00%；均摊按等权计算。与账号费用复用订单、对冲、收款及作废流程。
- 待确认起点可直接填写准确时间，或以充值时间查询母账号及影子账号的首请求；保存原始时间精度、来源记录和确认审计。
- 上游 CSV 使用 `Time`、`Model`、`Billed Cost`、`Input Tokens`、`Output Tokens`、`Cache Read Tokens`、`Cache Creation Tokens` 字段，可选 `Request ID` / `Upstream Request ID`；时间需含时区，金额按人民币处理。总额先按十进制求和再四舍五入到分。
- 模糊成本账号点击“结算”后，弹窗首屏提供上游账单导入；已导入账单的周期可从同一入口替换，手工准确成本不显示该入口。账单确认后留在当前弹窗并自动刷新分摊预览。
- 请求 ID 优先匹配，否则模型、四种 Token 数相同且时间差不超过 5 秒；候选须唯一且一对一。默认“比例分摊”将账单总成本按已匹配上游扣费的比例分摊，未匹配差异需显式确认。
- 手动选择“精准匹配”后，只汇总各用户匹配请求的上游扣费，未匹配费用不计入账单。保留上游总成本，另外展示本次计费与排除金额；按用户汇总后采用最大余数法处理分币尾差。两种模式均要求正金额匹配依据，范围外记录仍需确认排除。
- 账单替换成本并保留历史；同文件不重复计费，修改截止时间后需重新导入或确认沿用账单。正式出单冻结导入摘要、匹配与分摊依据。

## 部署

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

## 数据覆盖

每分钟流式核对全部源调用记录，分批暂存后在一个事务中发布；失败或重启不暴露半批结果。单账号预览和出单仅复核所选账号及周期，其他账号的迟到记录由独立的完整同步检测。按原 ID 去重，记录已删除时保留之前采集的账务依据。允许保存模糊起点；准确起点确认不改变实际数据覆盖范围。核对清理任务的删除数量与已留存记录，未采集或无法证明完整的范围标为缺口；涉及该范围的手动成本周期不能出单，预估不显示假零。采用上游账单时，比例分摊须由管理员确认本地差异；精准匹配只计入已匹配扣费，无需确认分摊未匹配费用。两种模式都保留缺口提示。源同步失败或中断时禁止结算；已出单周期发现消费变化或缺口标为差异，不自动改变金额。Sub2API 在首次接入前已经删除的记录无法恢复，正式回溯需核对原日志覆盖范围。

新部署不预填真实成本或创建欠款。管理员从“账号成本”配置付费或免费资源。

## 开发与验证

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
pnpm --dir frontend install --frozen-lockfile
pnpm --dir frontend test
pnpm --dir frontend build
cargo run --locked --bin settlement
```

`src/http.rs` 负责身份校验和接口，`src/service/` 负责周期、分摊、出单、对冲及查询，`src/source.rs` 和 `src/service/sync.rs` 负责只读源库和分批同步，`src/deletions.rs` 管理持久删除任务。SQLite 保持 WAL、外键检查和 FULL 持久化；请求和 CPU 计算并发均有上限。网络读取、CSV 匹配和分摊不占用账务写事务，提交时再次核验版本。

固定向量保存在 tests/golden/，静态 HTTP 契约、虚构旧账本 SQL 和合成会话位于 tests/fixtures/。所有验证由 Rust 执行，旧实现在本地归档的 legacy-python-final 标签中保留。

隔离演示：先构建前端，再运行 `cargo run --locked -p settlement-tools -- demo --netting-fixtures`。仅监听本机；入口 `/settlement/sso/start?token=demo-1&user_id=1&theme=light`，用户 2、3 同理。工具不进入运行镜像。

CI 执行 Rust 测试、Clippy、静态契约验证、前端测试与构建、Docker 检查。v* 标签发布源码与静态前端、Linux amd64 运行镜像和 SHA256SUMS。健康接口返回版本、提交修订及 engine: rust。

## 升级与回滚

升级沿用现有 `data/settlement.sqlite3` 和 `session.key`，迁移只增加辅助表、索引或兼容字段，不重算已出单金额。先在受保护的账本副本上设置 `SETTLEMENT_READ_ONLY=true`、`SETTLEMENT_EXTERNAL_WRITES=false`，验证迁移和查询结果；该模式拒绝业务写入并禁用外部删除。然后短暂停止服务并备份数据库、密钥和原镜像，切换后核对健康状态及财务记录。不要并行运行两个写入同一账本的服务。

v0.9.0 是最后一个已验证可与 Python 版双向回滚的版本；新版本不再提供旧解释器运行环境。

回滚时先切回上一个 Rust 版本镜像，保留当前兼容数据库和密钥。仅在确认切换后没有新的业务写入时，才能恢复升级前备份，避免丢失新订单或付款记录。同步中断会在下次完整核对时恢复；未完成的外部删除先核验账号状态，不直接重发删除请求。
