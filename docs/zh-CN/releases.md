# 发布与升级

[English](../releases.md) | **简体中文**

Bone 通过 [GitHub Releases](https://github.com/frelion/bone/releases) 发布。每个
release 都提供平台 archive 和 `SHA256SUMS`。

## 校验 archive

macOS / Linux 示例：

```bash
shasum -a 256 -c SHA256SUMS
tar -xzf bone-darwin-arm64.tar.gz
./bone/bone --version
```

Windows 请下载对应 zip，在解压前使用可信的本地工具校验 SHA-256。

可执行文件会携带对应的终端 helper 与本地语义搜索 native runtime；模型权重不会放进
release archive，需要语义搜索时在安装后运行 `bone setup`。

Bone 支持的运行时是 Bun。GitHub Release archive 包含独立 Bun 可执行文件；通过
package 或源码运行时需要 Bun 1.3.14 或更高版本。不支持使用 Node.js 运行 CLI。

## 发布策略

`vX.Y.Z` tag 会触发六平台 GitHub Release 流程：构建 native semantic runtime、编译
Bun binary、运行源代码校验，并在 checksum 验证后上传资产。

npm 当前尚未启用。Bone 公布专属 package scope 前，请不要将 npm package name 当作
升级通道。
