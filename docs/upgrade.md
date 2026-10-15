# 升级与回滚

升级沿用现有 `data/settlement.sqlite3` 和 `session.key`，迁移只增加辅助表、索引或兼容字段，不重算已出单金额。先在受保护的账本副本上设置 `SETTLEMENT_READ_ONLY=true`、`SETTLEMENT_EXTERNAL_WRITES=false`，验证迁移和查询结果；该模式拒绝业务写入并禁用外部删除。然后短暂停止服务并备份数据库、密钥和原镜像，切换后核对健康状态及财务记录。不要并行运行两个写入同一账本的服务。

v0.9.0 是最后一个已验证可与 Python 版双向回滚的版本；新版本不再提供旧解释器运行环境。

## 备份与迁移检查

在线备份使用 SQLite backup API，包含已提交的 WAL 内容；不会把数据库文件直接复制为不完整的快照。先创建权限受限的备份目录，再执行：

```sh
docker compose exec settlement /app/settlement backup --output /data/manual-backup.sqlite3
docker compose run --rm --no-deps settlement /app/settlement migrate --check
```

备份写入新的文件，完成完整性与外键检查后才原子发布，已有目标不会覆盖，文件权限为 0600。`migrate --check` 只读检查账本完整性、已应用版本及校验值，输出待执行迁移；不连接源库、不创建账本或会话密钥。实际迁移可通过 `migrate` 命令或服务启动执行。

首次使用版本化迁移或有新迁移时，服务在任何 schema 写入前自动备份到 `data/backups/`。备份失败、账本完整性异常、未知迁移版本或校验值不符都会停止迁移。迁移及其版本记录在同一事务提交。自动备份不会自动删除；运营者应按自己的保留策略转存与清理，并一同保存会话密钥文件。

回滚时先切回上一个 Rust 版本镜像，保留当前兼容数据库和密钥。仅在确认切换后没有新的业务写入时，才能恢复升级前备份，避免丢失新订单或付款记录。同步中断会在下次完整核对时恢复；未完成的外部删除先核验账号状态，不直接重发删除请求。

## 历史归档

从 v0.10.0 开始使用全新的 Rust 仓库历史。旧历史由维护者在原仓库的本地归档中保存，最终标签为 `legacy-python-final`；该标签不属于新的公开 Git 历史。历史发布说明与固定测试向量继续保留。迁移及以后的 schema 调整只增加辅助表、索引或兼容字段，不能重写历史订单、金额或快照。

回滚前先停止新版本，保存当前账本和密钥，记录最后一次业务写入时间。读取失败或版本不兼容时保留现状排查，不以覆盖账本作为默认修复。

## 会话密钥轮换

先停止服务并备份账本、`session.key`、存在时的 `session.keys.json` 与 `session.keys.pending.json`，再运行：

```sh
docker compose run --rm --no-deps settlement /app/settlement rotate-session-key --id rotation-01
docker compose up -d
```

每次使用新的密钥 ID。命令持有账本独占锁，在事务中重新加密会话；Cookie、CSRF、用户和有效期保持原值。`session.key` 始终保存当前密钥，旧密钥保留在权限 0600 的密钥集中，供已有会话兼容读取。密钥最多保留 32 个，达到上限时需要先审查和归档，不自动丢弃旧密钥。

中断时保留待完成文件；下次启动会根据其中的新旧密钥完成轮换。不要单独删除该文件或混用不同备份的数据库、密钥。若必须立即切回不支持密钥集的旧 Rust 镜像，应先让当前版本完成恢复，使所有会话均可由 `session.key` 解密。无法解密时启动会明确失败，账目不会被修改。

返回 [项目首页](../README.zh-CN.md)。
