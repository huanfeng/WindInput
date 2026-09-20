# 贡献指南

感谢您对清风输入法 (WindInput) 项目的关注！我们欢迎所有形式的贡献，包括 Bug 报告、功能建议和代码提交。

> **⚠️ alpha 阶段 PR 政策**
>
> 本项目目前处于 alpha 阶段，代码与文档变动频繁。为降低维护与冲突成本，**此阶段暂不接受仅包含文档改动或轻微改动的 Pull Request**，例如：
>
> - 纯文档变更（错别字、措辞润色、README/注释更新等）
> - 轻微改动（个别字符串、代码格式调整、无功能影响的小修小补等）
>
> 如发现文档错误或有改进建议，欢迎通过 [Issue](../../issues) 反馈，由维护者统一处理。功能性的 Bug 修复与新特性 PR 不受此限制，仍然欢迎提交。

## 签署 CLA（必须）

**所有贡献者在首次提交 Pull Request 前必须签署贡献者许可协议 (CLA)。**

这是为了确保项目的许可证管理和知识产权的一致性。流程如下：

1. 提交您的 Pull Request
2. CLA Assistant 机器人会自动在 PR 中发起签署请求
3. 在 PR 评论中回复：`I have read the CLA Document and I hereby sign the CLA`
4. 签署完成后，CLA 检查将自动通过

未签署 CLA 的 PR 将无法合并。完整协议内容请参阅 [CLA.md](CLA.md)。

## Bug 报告

请通过 [GitHub Issues](../../issues) 的 **Bug 反馈** 模板提交，并尽量包含以下信息：

- 操作系统与版本（如 Windows 11 24H2，或 macOS 15）
- 出现问题的应用程序名称
- 重现步骤、预期行为与实际行为
- 相关日志文件
  - Windows 安装版：`%LOCALAPPDATA%\WindInput\logs\`
  - Windows 便携版：`<程序目录>\localdata\logs\`
  - macOS：`~/Library/Logs/WindInput/`

## 功能建议

欢迎通过 [GitHub Issues](../../issues) 的 **功能建议** 模板提交。请描述您希望实现的功能、使用场景，如有参考实现请提供链接。

## 代码贡献

### 开发环境

通用要求：

- Rust stable 工具链（rustup 安装，含 `rustfmt` 与 `clippy` 组件）

Windows 额外需要：

- Visual Studio 2022（安装时勾选「使用 C++ 的桌面开发」组件，用于 MSVC 与 CMake）
- PowerShell 7+

macOS 额外需要：

- macOS 12+ 与 Xcode 15+（Swift 5.9 工具链）

### 构建

| 平台 | 命令 | 说明 |
|------|------|------|
| Windows | `.\scripts\dev.ps1` | 交互式菜单；`.\scripts\dev.ps1 1` 为 Release 全构建 |
| Windows | `.\scripts\dev.ps1 ci` | fmt 检查 + clippy + 全部测试（提 PR 前请运行） |
| macOS | `scripts/mac/dev.sh` | macOS 构建/调试菜单 |
| Linux | `scripts/dev.sh` | 交叉编译 Windows 产物（cargo-xwin） |

构建脚本会自动下载第三方词库数据（缓存于 `.cache/`）并生成完整数据目录。

> 配套的设置程序（wind-setting）、便携启动器（wind-portable）与安装器
> （wind-installer）为独立的未开源仓库，构建脚本在其不存在时会自动跳过，
> 不影响核心部分的构建。

### Git Hooks（首次克隆后建议激活）

仓库自带 `.githooks/pre-commit`（提交前自动跑 `cargo fmt --check`，避免
未格式化代码被提交后才在 CI 里暴露），默认不生效，需一次性激活：

```
.\scripts\dev.ps1 hooks    # Windows
scripts/dev.sh hooks       # Linux/macOS
```

（等价于 `git config core.hooksPath .githooks`，仅影响本地 clone，不会随仓库自动传播。）

### 提交规范

本项目使用 [Conventional Commits](https://www.conventionalcommits.org/zh-hans/) 规范：

```
<类型>(<范围>): <描述>

[可选的正文]
```

类型包括：

| 类型 | 说明 |
|------|------|
| `feat` | 新功能 |
| `fix` | Bug 修复 |
| `docs` | 文档变更 |
| `refactor` | 代码重构（不改变行为） |
| `perf` | 性能优化 |
| `test` | 测试相关 |
| `chore` | 构建/工具变更 |
| `style` | 格式化（如 `cargo fmt`） |

范围示例：`engine`、`coordinator`、`dict`、`config`、`ui`、`theme`、`rpc`、`tsf`、`macos`、`build`

### 代码风格

- **Rust**: 必须使用 `cargo fmt` 格式化（CI 会检查）；逻辑修改与 fmt 修改分开提交
- **C++**: 遵循项目现有代码风格（参考 `wind_tsf/src/`）
- **Swift**: 遵循项目 macOS 端现有风格（参考 `wind_macos/Sources/`）

### Pull Request 流程

1. Fork 本仓库并从 `main` 分支创建您的分支
2. 完成修改后运行 `.\scripts\dev.ps1 ci`（Windows）确保 fmt 检查、clippy 与测试通过
3. 修改了某 crate 的对外接口/导出常量/文件结构时，同步更新对应的 `AGENTS.md`
4. 按 PR 模板填写变更说明、测试情况与检查清单
5. 提交 PR 并等待 CLA 检查和代码审查
6. 根据审查意见修改后，PR 将被合并

## 项目结构

| 目录 | 说明 |
|------|------|
| `wind_input/` | Rust 核心服务（cargo workspace，18 个 crate） |
| `wind_tsf/` | Windows TSF 接口层（C++/CMake） |
| `wind_macos/` | macOS IMKit 输入法客户端（Swift/SwiftPM） |
| `data/` | 入库的方案、码表、主题等数据源文件 |
| `scripts/` | 构建与部署脚本 |
| `docs/` | 设计文档 |

各 crate 的职责索引与协作约定见根目录 [AGENTS.md](AGENTS.md)。

## 许可证

提交贡献即表示您同意您的贡献将按照项目的 [MIT 许可证](LICENSE) 进行授权。词库相关的第三方资源许可证请参阅 [NOTICE.md](NOTICE.md)。

此外，若您将本项目分支后公开发布，请更换项目名称与 logo 并注明为非官方分支。
该项不影响您对代码的任何权利，详见 [项目名称与标识使用约定](BRANDING.md)。
