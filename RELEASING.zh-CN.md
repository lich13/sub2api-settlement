# 发布流程

每次发布前完成以下步骤：

1. 在 `CHANGELOG.md` 顶部新增版本段落，使用 `v主版本.次版本.修订版本 · YYYY-MM-DD`，按“新增、调整、修复、兼容性、验证”组织内容。
2. 只写用户能判断影响的事实。不要写内部任务记录、个人信息、服务器路径、生产账号、真实账单或凭据。
3. 提交前确认仓库 Git 身份使用组织邮箱或 GitHub noreply 地址，不使用个人邮箱；检查 `git log -1 --format='%an <%ae>'`。
4. 更新 `Cargo.toml`、`xtask/Cargo.toml`、`frontend/package.json` 与 Docker 镜像标签中的版本号，并同步锁文件。
5. 执行后端测试、Clippy、前端测试与构建、Rust 演示账本 E2E、Docker 构建和 `git diff --check`，确认对应提交的 CI 全绿。
6. 用 `gitleaks` 检查工作树和 Git 历史，并检查发布包与运行镜像，确认没有生产 `.env`、`data/`、会话密钥、数据库及旁路文件、日志或临时文件；不含凭据的 `.env.example` 和明确标注的虚构演示图可以保留。
7. 创建带注释的 `v*` 标签并推送。CI 会从同一标签生成源码与前端发布包，并将对应的中文段落作为 GitHub Release 正文。
8. 发布后核对 Release 资产 SHA-256、测试提交、镜像 digest、Sigstore 签名与 SBOM。发布不自动连接或更新运行中的实例；另行部署时按升级文档备份，并核对实际版本与健康状态。

发布说明不自动生成英文模板，`CHANGELOG.md` 中缺少当前标签时 CI 应直接失败。
