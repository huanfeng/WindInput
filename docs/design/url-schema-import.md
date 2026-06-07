# URL Schema 一键导入（windinput://）

## 1. 背景与目标

为清风输入法提供一个通用的自定义 URL 协议 `windinput://`，让在线主题站、网页、文档等外部入口可以通过点击链接直接唤起**设置程序**并弹出导入确认框，实现"一键导入"。

本期落地**主题导入**全链路，并为后续的「导入输入方案」「导入用户词库」「导入扩展词库（类细胞词库）」预留解析分支与扩展点。

### 设计边界（重要）

- **协议注册与导入编排全部由设置程序 `wind_setting` 负责**，输入法核心服务（Windows `wind_input.exe` / macOS `wind_macos`）不参与任何协议/桌面集成逻辑，保持纯粹。
- 跨平台：同时支持 Windows 与 macOS。两平台共用导入逻辑与确认框 UI，仅"协议注册"和"URL 接收入口"按平台分文件实现。

## 2. 设计决策（已锁定）

| 维度 | 决策 |
|------|------|
| 数据传递方式 | URL 中传 **https 直链**（`?url=<urlencoded>`），程序下载后导入；不传内联 base64 数据 |
| URL 格式 | 路径式 `windinput://import/<kind>?url=...` |
| 协议注册（Windows） | **安装器安装时写** `HKCU\Software\Classes\windinput`（装完即用，无需手动开启）；设置程序 `OnStartup` 自愈对账（覆盖便携版/移动/升级换路径）；设置页（高级页）提供开关；**卸载注销由安装器负责** |
| 协议注册（macOS） | 声明式：`wails.json` 的 `info.protocols` → 打包进 `Info.plist` 的 `CFBundleURLTypes`，LaunchServices 自动登记；无运行时写入、无开关 |
| 注册责任方 | 全部 `wind_setting`，输入法服务不参与 |
| 安全级别 | 统一确认框 + 内容预览；确认框显著展示来源域名；沿用现有 1MB / 15s / v3 校验；本期不做来源白名单 |
| 实现范围 | 仅 `import/theme` 全链路；`schema`/`dict`/`extdict` 仅留解析分支 + TODO，不做 UI |
| 单实例策略 | 保持现有自研单实例（不引入 Wails `SingleInstanceLock`），仅在 IPC 上新增 `protocol\|<url>` 消息类型 |
| macOS 设置页开关 UX | 只读状态展示「已随应用自动注册（由系统管理）」 |

## 3. 架构总览

```
浏览器点击 windinput://import/theme?url=https://站点/x.yaml
        │
        ├─ Windows: 查 HKCU\Software\Classes\windinput → 启动
        │            wind_setting.exe "windinput://import/theme?url=..."
        │   ┌── 已有实例 → 现有 IPC(命名事件+临时文件) 透传 "protocol|<url>" → 本进程退出
        │   └── 无实例   → os.Args[1] 命中 → 启动后处理
        │
        └─ macOS: LaunchServices 路由到 wind_setting.app（运行中则激活，未运行则冷启动）
                   → Wails mac.OnUrlOpen(url) 回调
        ▼
   handleProtocolURL(ctx, rawURL)   [共享]
        │  protocol_url.go 解析 → ProtocolRequest{Kind, URL, Name, ...}
        │  时序兜底：前端未就绪则缓存 pending（沿用 startPage 模式）
        ▼
   前端 EventsOn("protocol-import") → ProtocolImportDialog.vue
        │  下载 + 解析元信息 → 展示：类型 / 名称 / 作者 / 条目数 / 来源域名 / 冲突提示
        ▼
   用户确认 → 调用既有 ImportThemeFromURL(url, force)
        ▼
   成功 Toast + 触发热重载（rpcClient.SystemNotifyReload）
```

## 4. 跨平台分层

| 层 | Windows | macOS | 共享 |
|----|---------|-------|------|
| 协议注册 | 运行时写/删/对账 `HKCU`，设置页开关 | `wails.json info.protocols` → `Info.plist CFBundleURLTypes` | 否 |
| URL 接收入口 | `os.Args[1]` + 现有命名事件 IPC（加 `protocol\|` 前缀） | Wails `mac.OnUrlOpen` 回调 | 否 |
| URL 解析 | `protocol_url.go` | 同左 | 是 |
| 投递前端 + pending 缓存 | `protocol_handler.go` | 同左 | 是 |
| 确认框 + 导入 | `ProtocolImportDialog.vue` + `ImportThemeFromURL` | 同左 | 是 |

## 5. 组件与文件清单

### 新建（共享，纯 Go / 平台无关）

- **`wind_setting/protocol_url.go`**
  - `type ProtocolRequest struct { Kind string; URL string; Name string }`（`Kind` ∈ `theme`/`schema`/`dict`/`extdict`）
  - `func ParseProtocolURL(raw string) (*ProtocolRequest, error)`：
    - 校验 scheme 必须为 `windinput`
    - 解析 host/path 为 `import/<kind>`，校验 `kind` 合法
    - 提取并 urldecode `url` 参数，校验必须 `https://`（入口收紧到仅 https）
    - 可选 `name` 参数（用于确认框展示，不可信，仅作提示）
    - 非法/缺参/未知 kind 返回明确 error
- **`wind_setting/protocol_handler.go`**
  - `func (a *App) handleProtocolURL(raw string)`：调用 `ParseProtocolURL`；成功则 `EventsEmit(ctx, "protocol-import", req)`；前端未就绪时存入 `a.pendingProtocol`
  - `func (a *App) ConsumePendingProtocol() *ProtocolRequest`（Wails 导出，前端 onMounted 主动拉取，消除 emit 早于 EventsOn 的竞争——沿用 `useUpdate.ts` 既有模式）

### 新建（平台分文件，沿用 `*_windows.go` / `*_darwin.go` 惯例）

- **`wind_setting/protocol_register_windows.go`**
  - `func RegisterProtocol() error`：写 `HKCU\Software\Classes\windinput`，含 `URL Protocol` 空值 + `shell\open\command = "<exe真实路径>" "%1"`
  - `func UnregisterProtocol() error`：删该键
  - `func ProtocolStatus() (registered bool, command string)`：读回对账
  - `func SelfHealProtocol()`：OnStartup 调用——缺失或 command 与当前 exe 路径不符则重写（解决便携版移动 / 升级换路径）
  - 命令行支持 `--register-protocol` / `--unregister-protocol`（供脚本 / 便携版手动清理）
- **`wind_setting/protocol_register_darwin.go`**
  - `RegisterProtocol`/`UnregisterProtocol` no-op；`ProtocolStatus` 返回 `registered=true, command="由系统管理（随应用注册）"`；`SelfHealProtocol` no-op

### 改

- **`wind_setting/main.go`**
  - 解析阶段：识别 `os.Args[1]` 以 `windinput://` 开头（Windows 主路径）→ 经单实例透传或留给本进程
  - `OnStartup`：调用 `SelfHealProtocol()`（darwin 为 no-op）；若有待处理 URL 调 `handleProtocolURL`
  - macOS：在 `options.App.Mac.OnUrlOpen` 注入 `func(url string){ app.handleProtocolURL(url) }`
- **`wind_setting/singleton_windows.go`**
  - `sendPageToExisting` / IPC 监听支持 `protocol|<rawURL>` 消息：收到后激活窗口并调用 `app.handleProtocolURL(rawURL)`（由它统一解析并 emit `protocol-import`，前端只监听这一个事件）
- **`wind_setting/wails.json`**
  - 增加：
    ```json
    "info": { "protocols": [ { "scheme": "windinput", "role": "Viewer" } ] }
    ```
- **`wind_setting/app.go`**（App 结构）
  - 新增字段 `pendingProtocol *ProtocolRequest`
- **`wind_setting/frontend/src/App.vue`**
  - `EventsOn("protocol-import", req => 打开 ProtocolImportDialog 并 Show())`
  - `onMounted` 调 `ConsumePendingProtocol()` 拉取冷启动缓存
  - `onUnmounted` `EventsOff("protocol-import")`
- **设置页（高级页 / `app_advanced.go` 对应前端页）**
  - 新增「windinput:// 链接关联」区块：
    - Windows：开关（绑定 `RegisterProtocol`/`UnregisterProtocol`）+ 当前状态展示
    - macOS：只读信息「已随应用自动注册（由系统管理）」
- **`installer/nsis/WindInput.nsi`**
  - 安装段：写 `HKCU\Software\Classes\windinput`——`URL Protocol` 空值 + `shell\open\command = "$INSTDIR\wind_setting.exe" "%1"`，使安装完成后**无需手动开启即可用**（沿用已有 `WriteRegStr HKCU` 先例，见 `WindInput.nsi:996`）
  - 卸载段：删 `HKCU\Software\Classes\windinput`，由**安装器负责清理**
  - 注册闭环：安装器装时写 / 卸时删；设置程序运行时自愈兜底（便携版/移动/升级）+ 设置页可开关
  - 仅清当前用户键；多用户场景下其他用户残留为指向失效 exe 的死键，无安全/功能危害

### 新建（前端）

- **`wind_setting/frontend/src/components/ProtocolImportDialog.vue`**
  - 通用导入确认框；本期实现 `theme` 分支：下载 → 解析元信息 → 展示 → 确认调用 `ImportThemeFromURL`
  - `schema`/`dict`/`extdict` 分支占位（显示「暂未支持」或留 TODO），保证 UI 可扩展

### 复用（零改动）

- `app_theme.go`（`ImportThemeFromURL` / `importThemeFromContent` / 冲突检测 / `sanitizeThemeSlug`）
- `app_schema.go` / `app_dict_import.go`（后续 kind 落地时复用，平台无关）

## 6. URL 约定

```
# 本期实现
windinput://import/theme?url=<urlencoded https 直链>[&name=<显示名>]

# 预留（仅解析分支 + TODO，本期不做 UI）
windinput://import/schema?url=...
windinput://import/dict?url=...&schema=<目标方案ID>
windinput://import/extdict?url=...
```

约定细则：

- scheme 固定 `windinput`，大小写不敏感由系统处理，程序内统一小写比较。
- `url` 参数必须为 `https://`（比设置页内 `ImportThemeFromURL` 允许 http 更严，因 URL schema 入口可被任意网站触发）。
- `name` 参数仅用于确认框预填展示，**不可信**，真实名称以下载内容解析出的 `meta.name` 为准。
- 未知 `kind` / 缺 `url` / 非 https → 前端弹明确错误提示，不静默吞掉。

## 7. 错误处理与安全

- **统一确认框 + 内容预览**：所有导入在落盘前必须经用户确认；确认框展示下载解析出的元信息 + 来源域名，供用户判断可信度。
- **冲突处理**：复用 `ImportThemeResult.Conflict`，同名时提示「覆盖 / 取消」，覆盖走 `force=true`。
- **大小 / 超时 / 校验**：沿用 `ImportThemeFromURL` 的 1MB 上限、15s 超时、LightweightManager v3 全链校验。
- **路径安全**：主题落盘沿用 `sanitizeThemeSlug` + slug 校验，防路径遍历。
- **来源白名单**：本期不做（按选定的"统一确认框"级别）；确认框展示域名作为软性提示。后续如需可在 `protocol_handler.go` 增加可选白名单校验，不影响现有结构。

## 8. 测试策略

- **`protocol_url_test.go`**：URL 解析单测——合法主题链接、缺 `url`、非 https、未知 kind、urlencode 还原、scheme 错误、`name` 提取。
- **`protocol_register_windows_test.go`**：注册 → 读回 → 注销 对账（用临时 HKCU 子键隔离，避免污染真实注册表）；自愈逻辑（路径不符触发重写）。
- **既有 `app_theme_test.go`**：导入管线已覆盖，无需重测。
- **手动 E2E**：
  - Windows：构造 `windinput://import/theme?url=...` 测试页，浏览器点击验证「冷启动」「已开实例透传」两条路径 + 确认框 + 落盘 + 热重载。
  - macOS：打包 .app（含 `info.protocols`），`open "windinput://import/theme?url=..."` 验证 OnUrlOpen 路由 + 冷启动 pending 缓存时序。

## 9. 验收标准

1. Windows 安装版：安装完成后**无需打开设置**，浏览器点击 `windinput://import/theme?url=<合法直链>` 即能弹出确认框并成功导入主题、触发热重载。便携版：设置程序至少打开一次（自愈注册）后同样可用。
2. macOS 打包 .app 后，`open windinput://...` 能路由到设置程序并完成同样流程（含未运行时的冷启动）。
3. 设置页「链接关联」区块：Windows 可开关并正确读回状态；macOS 显示只读「由系统管理」。
4. 非法 URL（缺参 / 非 https / 未知 kind）给出明确错误提示，不崩溃、不静默。
5. `schema`/`dict`/`extdict` 链接被识别但提示「暂未支持」，不报解析错误。
6. 卸载（Windows）后当前用户的 `HKCU\Software\Classes\windinput` 被清除。

## 10. 未来扩展点

- 落地 `schema` / `dict` / `extdict` 的确认框分支与导入调用（复用 `PreviewImportSchema`/`ConfirmImportSchema`、`PreviewImportFile`/`ExecuteImport`）。
- 可选来源域名白名单 / 信任管理 UI。
- 主题站侧生成 `windinput://import/theme?url=...` 按钮的对接文档。
