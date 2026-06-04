# 贡献指南

感谢您对清风输入法 (WindInput) 项目的关注！我们欢迎所有形式的贡献，包括 Bug 报告、功能建议、文档改进和代码提交。

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

请通过 [GitHub Issues](../../issues) 的 **Bug 报告** 模板提交，并尽量包含以下信息：

- 操作系统与版本（如 Windows 11 23H2，或 macOS 14）
- 出现问题的应用程序名称
- 重现步骤
- 预期行为与实际行为
- 相关日志文件
  - Windows：`%LOCALAPPDATA%\WindInput\logs\`
  - macOS：`~/Library/Logs/WindInput/wind_input.log`

## 功能建议

欢迎通过 [GitHub Issues](../../issues) 的 **功能建议** 模板提交。请描述：

- 您希望实现的功能
- 该功能的使用场景
- 如果有参考实现，请提供链接

## 代码贡献

### 开发环境

请参阅 [开发文档](docs/DEVELOPMENT.md) 了解详细的开发环境搭建和构建流程（含 CMake、PowerShell、.NET、macOS 等说明）。

基本要求（通用）：

- Go 1.25+（构建完整套件含设置工具需 1.26+）
- Wails v2.12+ CLI
- Node.js + pnpm

Windows 额外需要：

- Visual Studio 2017+（安装时勾选「使用 C++ 的桌面开发」与「.NET 桌面开发」组件）
- CMake 3.15+（VS 自带）
- PowerShell 7+

macOS 额外需要：

- macOS 12+ 与 Xcode 15+（Swift 5.9 工具链）

### 提交规范

本项目使用 [Conventional Commits](https://www.conventionalcommits.org/zh-hans/) 规范：

```
<类型>(<范围>): <描述>

[可选的正文]

[可选的脚注]
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

范围示例：`engine`, `tsf`, `macos`, `ui`, `setting`, `dict`, `ipc`, `schema`, `portable`, `build`

### Pull Request 流程

1. Fork 本仓库并创建您的分支（从 `main` 分支）
2. 确保代码通过编译：Windows 用 `.\build_all.ps1`，macOS 用 `./dev_mac.sh 1`
3. Go 代码请运行 `go fmt`
4. 前端代码请运行格式化
5. 修改了某目录对外接口/导出常量/文件结构时，同步更新该目录的 `AGENTS.md`
6. 按 PR 模板填写变更说明、测试情况与检查清单
7. 提交 PR 并等待 CLA 检查和代码审查
8. 根据审查意见修改后，PR 将被合并

### 代码风格

- **Go**: 遵循标准 Go 代码风格，使用 `go fmt` 格式化
- **C++**: 遵循项目现有的代码风格（参考 `wind_tsf/src/` 中的代码）
- **Swift**: 遵循项目 macOS 端现有风格（参考 `wind_macos/Sources/` 中的代码）
- **Vue/TypeScript**: 遵循项目前端的格式化配置

## 项目结构

关于项目架构和模块说明，请参阅 [开发文档](docs/DEVELOPMENT.md)。

## 许可证

提交贡献即表示您同意您的贡献将按照项目的 [MIT 许可证](LICENSE) 进行授权。词库相关的第三方资源许可证请参阅 [NOTICE.md](NOTICE.md)。
