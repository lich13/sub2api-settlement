# 发布流程

每次发布前完成以下步骤：

1. 在 `CHANGELOG.md` 顶部新增版本段落，使用 `v主版本.次版本.修订版本 · YYYY-MM-DD`，按“新增、调整、修复、兼容性、验证”组织内容。
2. 只写用户能判断影响的事实。不要写内部任务记录、个人信息、服务器路径、生产账号、真实账单或凭据。
3. 提交前确认仓库 Git 身份使用组织邮箱或 GitHub noreply 地址，不使用个人邮箱；检查 `git log -1 --format='%an <%ae>'`。
4. 更新 `Cargo.toml`、`xtask/Cargo.toml` 和 `frontend/package.json` 的版本号，保持版本一致。
5. 执行后端测试、前端测试、TypeScript 检查、Vite 构建、Docker 构建和 `git diff --check`。
6. 检查 `git ls-files` 和发布包内容，确认没有生产 `.env`、`data/`、`session.key`、数据库、日志、截图或临时文件；不含凭据的 `.env.example` 模板可以保留。
7. 创建带注释的 `v*` 标签并推送。CI 会从同一标签生成源码与前端发布包，并将对应的中文段落作为 GitHub Release 正文。
8. 发布后核对 Release 资产 SHA-256、版本健康检查和公开入口；部署前先备份生产 SQLite，验收完成后清理临时文件。

发布说明不自动生成英文模板，`CHANGELOG.md` 中缺少当前标签时 CI 应直接失败。
