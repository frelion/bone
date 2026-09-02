# 发布与升级

[English](../releases.md) | **简体中文**

Bone 通过 [GitHub Releases](https://github.com/frelion/bone/releases) 发布。每个
release 都为每个平台提供一个独立可执行文件和 `SHA256SUMS`。

## 校验可执行文件

macOS / Linux 示例：

```bash
shasum -a 256 -c SHA256SUMS
chmod +x bone-darwin-arm64
./bone-darwin-arm64 --version
```

Windows 请下载对应的 `.exe`，使用可信的本地工具校验 SHA-256 后直接运行。

可执行文件内已包含对应的终端 helper、主题、模板、WASM、剪贴板 addon 与本地语义搜索
native runtime，不依赖同目录下的其他文件。首次启动时，原生运行时文件会自动释放到
用户缓存。模型权重不会放进 release，需要语义搜索时在安装后运行 `bone setup`。

Bone 支持的运行时是 Bun。GitHub Release 包含独立 Bun 可执行文件；通过
package 或源码运行时需要 Bun 1.3.14 或更高版本。不支持使用 Node.js 运行 CLI。

## 发布策略

`vX.Y.Z` tag 会触发六平台 GitHub Release 流程：构建 native semantic runtime、编译
Bun binary、运行源代码校验，并在 checksum 验证后上传资产。

npm 当前尚未启用。Bone 公布专属 package scope 前，请不要将 npm package name 当作
升级通道。
