<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-08 | Updated: 2026-04-08 -->

# NSIS 安装脚本目录 (installer/nsis/)

## 用途

NSIS (Nullsoft Scriptable Install System) 配置文件，定义 Windows 图形化安装程序的界面、步骤和文件复制逻辑。通过 `build_nsis.ps1` 调用 `makensis.exe` 编译此脚本生成 `.exe` 安装程序。

## 主要文件

| 文件 | 描述 |
|------|------|
| `WindInput.nsi` | 主安装脚本：使用 MUI2 主题、定义安装向导、文件包含、注册表操作 |

## 子目录

无

## NSIS 脚本说明

### 脚本结构

```
1. 变量定义和编译时检查
   - APP_VERSION、APP_VERSION_NUM（由 build_nsis.ps1 传入）
   - BUILD_DIR、OUTPUT_DIR 路径
   - 文件存在性验证（wind_tsf.dll、wind_input.exe、wind_setting.exe、data/dict/.../8105.dict.yaml 等；不检查 wind_dwrite.dll）

2. 界面配置（MUI2）
   - 欢迎页面、许可证、安装位置、组件选择、安装进度、完成页面

3. 安装过程（.onInit、Section）
   - 检查管理员权限
   - 复制 DLL、EXE、词库文件
   - 注册 COM 组件
   - 配置注册表

4. 卸载过程（un.Section）
   - 停止服务进程
   - 注销 COM 组件
   - 删除安装目录和注册表
```

### 重要变量

```nsi
APP_VERSION         # 版本号，如 "0.1.0"
APP_VERSION_NUM     # 数值版本，如 "0.1.0.0"（用于文件属性）
APP_NAME           # "清风输入法"
APP_DIRNAME        # "WindInput"
BUILD_DIR          # "..\..\build"（相对于 nsis 目录）
OUTPUT_DIR         # "..\..\build\installer"（输出安装程序目录）
UNINST_KEY         # 卸载注册表键：Software\Microsoft\Windows\CurrentVersion\Uninstall\清风输入法
```

### 关键部分

#### 1. 编译时文件检查

```nsi
!if /FileExists "${BUILD_DIR}\wind_tsf.dll"
!else
!error "Missing file: ${BUILD_DIR}\wind_tsf.dll. Run build_all.ps1 first."
!endif
```

确保所有编译产物已生成，防止生成不完整的安装程序。

#### 2. 安装向导页面

使用 MUI2 宏定义标准向导：
- `MUI_PAGE_WELCOME` - 欢迎
- `MUI_PAGE_LICENSE` - 许可证
- `MUI_PAGE_DIRECTORY` - 安装位置选择
- `MUI_PAGE_COMPONENTS` - 组件选择
- `MUI_PAGE_INSTFILES` - 安装进度
- `MUI_PAGE_FINISH` - 完成

#### 3. 文件复制

```nsi
SetOutPath "$INSTDIR"
File /a "${BUILD_DIR}\wind_tsf.dll"
File /a "${BUILD_DIR}\wind_input.exe"
File /a "${BUILD_DIR}\wind_setting.exe"
; wind_dwrite.dll 已移除，不再复制

; 默认配置文件
SetOutPath "$INSTDIR\data"
File /a "${BUILD_DIR}\data\config.yaml"

; Schema 配置文件
SetOutPath "$INSTDIR\data\schemas"
File /a "${BUILD_DIR}\data\schemas\*.schema.yaml"
```

#### 4. COM 注册

```nsi
ExecWait "regsvr32.exe /s $INSTDIR\wind_tsf.dll"
```

注册 wind_tsf.dll 为 Windows COM 组件，使 TSF 框架能识别。

#### 5. 启动项配置

```nsi
WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" \
    "WindInput" "$INSTDIR\wind_input.exe"
```

将 wind_input.exe 添加到开机自启动。

## 工作指南

### 修改安装步骤

编辑 `WindInput.nsi` 时：

1. **增加新的文件**：在 `SetOutPath` 后添加 `File` 指令
   ```nsi
   SetOutPath "$INSTDIR\dict"
   File /a "${BUILD_DIR}\dict\*.*"
   ```

2. **添加新的注册表项**：使用 `WriteRegStr` / `WriteRegDWord`
   ```nsi
   WriteRegStr HKLM "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
   ```

3. **修改版本号**：不要直接编辑脚本中的 `!define APP_VERSION`，应通过 `build_nsis.bat` 的参数传入
   ```bash
   build_nsis.bat 0.2.0  # 自动替换版本号
   ```

### 版本号传入机制

`build_nsis.ps1` 通过 `/D` 参数动态传入版本号：

```powershell
& makensis /DAPP_VERSION=$Version /DAPP_VERSION_NUM=$VersionNum WindInput.nsi
```

这样避免了手工修改脚本中的硬编码版本号。

### 测试安装程序

```powershell
# 1. 构建安装程序
installer\build_nsis.ps1 -Version 0.1.0

# 2. 运行生成的安装程序
build\installer\清风输入法-0.1.0-Setup.exe

# 3. 按向导完成安装

# 4. 验证安装
# - 检查 Program Files\WindInput 中的文件
# - 检查输入法列表中是否有"清风输入法"
# - 打开文本编辑器尝试输入

# 5. 测试卸载
# - 控制面板 → 程序和功能 → 清风输入法 → 卸载
```

## 依赖关系

### 内部

- `../build_nsis.ps1` - 调用脚本
- `../../build/` - 编译产物目录（wind_tsf.dll、wind_input.exe、wind_setting.exe；不含 wind_dwrite.dll）
- `../../build/data/dict/` - 词库文件
- `../../build/data/schemas/` - schema 配置文件（*.schema.yaml）
- `../../build/data/config.yaml` - 默认配置文件

### 外部

- **NSIS 3.x** - `makensis.exe` 必须在 PATH
- **MUI2.nsh** - NSIS 标准库（包含在 NSIS 安装包中）
- **FileFunc.nsh** - 文件操作库
- **LogicLib.nsh** - 逻辑操作库
- **x64.nsh** - 64 位系统检查库

## 常见问题

### makensis 找不到

确保 NSIS 已安装且路径在环境变量 PATH 中：
```bash
# 验证
where makensis

# 如果没有，添加到 PATH
# 默认安装路径：C:\Program Files (x86)\NSIS
```

### 生成的安装程序无法运行

1. 检查文件存在性：
   ```bash
   ls -la build\wind_tsf.dll build\wind_input.exe build\wind_setting.exe
   ```

2. 检查文件路径是否正确（特别是 `${BUILD_DIR}` 变量）

3. 重新运行 `build_all.ps1` 确保所有编译产物已生成

### 安装后提示"缺少文件"

运行安装程序时缺少词库或其他数据文件。解决方案：
1. 确保 `build/dict/` 目录中有词库文件
2. 运行 `installer\build_nsis.bat --skip-build` 重新生成（跳过编译，直接用现有产物）

<!-- MANUAL: -->
