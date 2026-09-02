# Bone

[English](README.md) | **简体中文**

Bone 是一个本地优先的 Coding Agent。它提供 Side 会话栏、
可在后台继续运行的并发会话、可视化 Provider 配置、任务模型路由，以及完全本地的
语义记忆与会话搜索。

```bash
bone
```

通过 `/settings` 配置 Provider 与模型。首次需要本地语义搜索时，显式运行一次
`bone setup` 下载模型；正常启动 **不会** 自动下载。

请阅读[中文文档](docs/zh-CN/README.md)，了解安装、多会话、设置中心、本地
Memory/Search 和 GitHub Release。

## 发布

Bone 目前通过 [GitHub Releases](https://github.com/frelion/bone/releases) 发布。
每个 tag release 都包含 macOS、Linux、Windows 的原生 Bun 二进制与 SHA-256 校验和。
npm 发布将在 Bone 拥有独立的 package scope 和升级通道后再启用。

## 开发

```bash
bun install --ignore-scripts
bun run build
bun run check
bun run test
```

日常使用源码开发时，可以运行 `bun run dev:install-hook` 为主 checkout 安装本地
post-commit 和 post-merge hook。提交或合并后它会构建当前平台的 Bun 二进制，
并在构建成功后原子切换本地 `bone` 命令；构建失败时保留上一版。其他 worktree
中的事件不会触发安装。单次跳过可设置 `BONE_SKIP_LOCAL_INSTALL=1`。运行
`bun run dev:uninstall-hook` 会恢复该 clone 原来的 Git hook 路径与 `bone` 命令。

源码是 Bun workspace monorepo。在 GitHub Release 阶段，内部 package 名称只是
实现细节，Bone 仓库不会发布它们。Bun 1.3.14 或更高版本是唯一需要的 JavaScript
运行时与工具链；release archive 提供独立 Bun 可执行文件。

## 供应链策略

- 直接依赖固定精确版本，`bunfig.toml` 强制两天 registry 年龄门槛。
- CI 使用 `--ignore-scripts` 安装依赖，并执行 build、check 和 test。
- `bun.lock` 是唯一依赖锁文件，release 前通过 frozen install 校验。
- release 资产附带 SHA-256；原生 sidecar 在打包前进行完整性检查。

## License

MIT
