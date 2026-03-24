# 贡献指南

感谢您对清风输入法 (WindInput) 项目的关注！我们欢迎所有形式的贡献，包括 Bug 报告、功能建议、文档改进和代码提交。

## 签署 CLA（必须）

**所有贡献者在首次提交 Pull Request 前必须签署贡献者许可协议 (CLA)。**

这是为了确保项目的许可证管理和知识产权的一致性。流程如下：

1. 提交您的 Pull Request
2. CLA Assistant 机器人会自动在 PR 中发起签署请求
3. 在 PR 评论中回复：`I have read the CLA Document and I hereby sign the CLA`
4. 签署完成后，CLA 检查将自动通过

未签署 CLA 的 PR 将无法合并。完整协议内容请参阅 [CLA.md](CLA.md)。

## Bug 报告

提交 Bug 时请包含以下信息：

- Windows 版本（如 Windows 11 23H2）
- 出现问题的应用程序名称
- 重现步骤
- 预期行为与实际行为
- 相关日志文件（位于 `%LOCALAPPDATA%\WindInput\logs\`）

## 功能建议

欢迎通过 Issue 提交功能建议。请描述：

- 您希望实现的功能
- 该功能的使用场景
- 如果有参考实现，请提供链接

## 代码贡献

### 开发环境

请参阅 [开发文档](docs/DEVELOPMENT.md) 了解详细的开发环境搭建和构建流程。

基本要求：

- Go 1.24+
- Visual Studio 2017+（含 C++ 桌面开发工具）
- CMake 3.15+
- Wails v2 CLI
- Node.js + pnpm

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

范围示例：`engine`, `tsf`, `ui`, `setting`, `dict`, `ipc`, `schema`, `build`

### Pull Request 流程

1. Fork 本仓库并创建您的分支（从 `main` 分支）
2. 确保代码通过编译：`.\build_all.ps1`
3. Go 代码请运行 `go fmt`
4. 前端代码请运行格式化
5. 提交 PR 并等待 CLA 检查和代码审查
6. 根据审查意见修改后，PR 将被合并

### 代码风格

- **Go**: 遵循标准 Go 代码风格，使用 `go fmt` 格式化
- **C++**: 遵循项目现有的代码风格（参考 `wind_tsf/src/` 中的代码）
- **Vue/TypeScript**: 遵循项目前端的格式化配置

## 项目结构

关于项目架构和模块说明，请参阅 [开发文档](docs/DEVELOPMENT.md)。

## 许可证

提交贡献即表示您同意您的贡献将按照项目的 [MIT 许可证](LICENSE) 进行授权。词库相关的第三方资源许可证请参阅 [NOTICE.md](NOTICE.md)。
