<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-08 | Updated: 2026-04-08 -->

# scripts/ - 构建辅助和工具脚本

## Purpose

项目构建辅助脚本和诊断工具目录。这些脚本不参与主构建流程，供开发者手动调用，用于版本管理、系统诊断等任务。

## Key Files

| File | Description |
|------|-------------|
| `bump-version.ps1` | 版本号管理脚本：读取 VERSION 文件，按 major/minor/patch/prerelease 规则递增版本号，同步更新所有版本号引用文件（VERSION、go.mod、CMakeLists.txt 等） |
| `check_band.ps1` | DWM Window Band 诊断工具：枚举系统窗口并显示各窗口的 Band 等级，用于调试 Win11 开始菜单候选框 z-order 问题和验证 HostWindow 机制 |

## Usage

### bump-version.ps1

```powershell
# 升级补丁版本（0.1.0 → 0.1.1）
scripts\bump-version.ps1 -Version patch

# 升级次版本（0.1.0 → 0.2.0）
scripts\bump-version.ps1 -Version minor

# 升级主版本（0.1.0 → 1.0.0）
scripts\bump-version.ps1 -Version major

# 设置预发布标识（0.1.0 → 0.1.0-alpha）
scripts\bump-version.ps1 -Version patch -Preid alpha
```

### check_band.ps1

```powershell
# 倒计时 8 秒后扫描（先打开开始菜单再运行）
scripts\check_band.ps1

# 立即扫描
scripts\check_band.ps1 -Now

# 持续监控（Ctrl+C 停止）
scripts\check_band.ps1 -Loop

# 显示所有窗口（含 Band=0/1 的普通窗口）
scripts\check_band.ps1 -All
```

## For AI Agents

### Working In This Directory

- `bump-version.ps1` 会修改多个文件中的版本号，运行前确认当前工作区干净
- `check_band.ps1` 是只读诊断工具，不修改系统状态，可随时运行
- 这两个脚本均不需要管理员权限
- 新增脚本时保持与现有文件相同的 PowerShell 编码风格（UTF-8 with BOM，`$ErrorActionPreference = "Stop"`）

## Dependencies

### Internal
- `bump-version.ps1` 依赖项目根目录的 `VERSION` 文件
- `check_band.ps1` 通过 P/Invoke 调用 `user32.dll` 的 `GetWindowBand`（与 HostWindow 使用相同的非公开 API）

### External
- PowerShell 5.1+ 或 PowerShell 7+
- `check_band.ps1` 需要 Windows 10/11（GetWindowBand API 仅在 Win10+ 存在）

<!-- MANUAL: -->
