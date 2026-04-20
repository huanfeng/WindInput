<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-20 | Updated: 2026-04-20 -->

# release-notes/ - 发布说明与版本历史

## 用途

用户发布说明（Release Notes）目录。包含各版本的功能清单、Bug 修复、已知问题，供用户和开发者了解版本变化。

## 文件结构

| 文件 | 描述 |
|------|------|
| `header.md` | 发布说明模板头部（自动填充版本号、日期等） |
| `footer.md` | 发布说明模板尾部（致谢、链接等） |
| `v0.1.0-alpha.md` | v0.1.0-alpha 版本发布说明 |

## 发布说明模板

### 标准结构

```markdown
# 版本号 - 日期

## 新增功能
- 功能 A
- 功能 B

## 改进
- 性能优化
- UI 改进

## Bug 修复
- 问题 #123
- 问题 #456

## 已知问题
- 问题说明

## 感谢
贡献者名单（如适用）

---

[下载](#) | [更新日志](#) | [GitHub Releases](#)
```

## 工作指南

### 添加新的发布说明

1. **创建文件**：`v<VERSION>.md`，按语义化版本命名（如 `v0.2.0.md`）

2. **使用模板**：
   ```bash
   # 复制模板
   cp release-notes/v0.1.0-alpha.md release-notes/v0.2.0.md
   # 编辑版本号、日期、内容
   ```

3. **内容清单**：
   - 新增功能（简明扼要，面向用户）
   - 改进项目（性能、UI、易用性）
   - Bug 修复（Ref 相关 Issue）
   - 已知问题（影响使用的已知限制）
   - 升级指南（如有破坏性变更）

4. **示例**：

   ```markdown
   # v0.2.0 - 2026-04-20
   
   ## 新增功能
   - 五笔拼音混输支持
   - 自定义快捷键配置
   - 导入词库功能
   
   ## 改进
   - 拼音候选排序算法优化 (+15% 命中率)
   - 启动速度加快 (-200ms)
   - Win11 开始菜单候选框 z-order 问题修复
   
   ## Bug 修复
   - 修复长句子无法上屏的问题 (#42)
   - 修复配置重置后某些设置丢失的问题 (#48)
   - 修复高 DPI 显示器下候选框大小错误的问题 (#53)
   
   ## 已知问题
   - 某些虚拟机环境下 TSF DLL 加载失败（需安装 .NET Framework）
   - 同时安装多个输入法时可能出现切换延迟
   
   ## 升级说明
   无破坏性变更。直接升级即可。
   
   ---
   
   感谢以下贡献者：xxxx（排序）
   ```

### 自动生成

可在 CI/CD 流程中自动生成发布说明：
1. 解析 Commit 日志（使用 `git log --oneline` 或 Conventional Commits）
2. 分类为 feat/fix/perf 等
3. 插入 header.md 和 footer.md
4. 输出最终版本说明

**示例脚本**（PowerShell）：
```powershell
$commits = git log v0.1.0..HEAD --oneline
$features = $commits | Where-Object { $_ -match "^.*feat:" }
$fixes = $commits | Where-Object { $_ -match "^.*fix:" }
# ... 生成 Markdown 输出
```

## 依赖关系

### 内部
- `../AGENTS.md` - 文档目录索引
- `../DEVELOPMENT.md` - 开发流程（版本号管理）
- 项目 VERSION 文件
- Git commit 历史

### 外部
- GitHub Releases API（如需发布到 GitHub）
- 用户手册（可链接至此）

<!-- MANUAL: -->
