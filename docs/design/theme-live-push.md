# 主题在线推送设计文档

**日期：** 2026-06-05  
**状态：** 已批准，待实现  
**相关仓库：** WindInput（主仓）、WindInputThemeEditor（Web 编辑器）

---

## 背景

WindInput 已有完整的主题导入（文件/文本）和热更新链路。WindInputThemeEditor 是独立的 Web 编辑器，目前两者无实时通信。本功能在 `wind_setting` 内嵌一个轻量 HTTP 服务，允许 Web 编辑器一键推送主题到本地输入法并触发刷新。

---

## 目标

- 用户在 Web 编辑器中编辑完主题后，能一键推送到本地输入法并立即生效
- Web 编辑器能拉取本地已安装的主题列表，选择某个主题加载继续编辑
- 操作简单：设置页开一个开关，编辑器填写本地地址即可

## 非目标

- 不需要云端中继或配对 token
- 不需要实时双向同步（本地变化不主动推送到 Web 编辑器）
- 不覆盖 WindInputThemeEditor 仓库的具体实现（仅定义 API 契约）

---

## 架构

```
WindInputThemeEditor (浏览器)
        │
        │  HTTP REST  (localhost:29731 起，最多尝试 29733)
        │
        ▼
wind_setting 内嵌 HTTP Server  [新增 theme_server.go]
  ├── GET  /api/themes          ← 拉取本地主题列表
  ├── GET  /api/theme/:slug     ← 拉取单个主题 YAML 内容
  └── POST /api/theme/push      ← 推送主题 YAML 到本地
        │
        ▼  复用现有 ImportThemeFromText() 管线
wind_setting/app_theme.go
        │
        ▼  现有命名管道 RPC
wind_input coordinator → 热重载主题
```

---

## 组件详细设计

### 1. HTTP 服务（`wind_setting/theme_server.go`）

**启停：**
- `ThemeServer` 结构体，持有 `*http.Server` 和监听端口
- `Start()` 方法：从端口 29731 开始尝试，最多尝试 3 个端口（29731/29732/29733），均失败则返回错误
- `Stop()` 方法：优雅关闭（`http.Server.Shutdown`，超时 3s）
- 服务生命周期绑定到 `wind_setting` 进程；设置页关闭时服务随之停止

**CORS：**
- 固定允许来源白名单（Web 编辑器生产域名 + `http://localhost:*` 开发域名）
- 允许方法：GET、POST、OPTIONS
- 本地 localhost 已是受信任环境，无需 token

**API 端点：**

| 端点 | 方法 | 请求体 | 响应 |
|------|------|--------|------|
| `/api/themes` | GET | — | `[{slug, name, hasLightDark, isUserTheme}]` |
| `/api/theme/:slug` | GET | — | `{slug, name, yaml}` |
| `/api/theme/push` | POST | `{yaml: string, force: bool}` | `{imported, reloaded, themeName}` 或错误 |

**`POST /api/theme/push` 状态码：**

| 状态码 | 含义 |
|--------|------|
| 200 | 推送并热重载成功，`{imported:true, reloaded:true, themeName:"..."}` |
| 200 | wind_input 未运行时，主题已写入文件但未重载，`{imported:true, reloaded:false, themeName:"..."}` |
| 400 | YAML 格式错误，`{error: "invalid yaml: ..."}` |
| 409 | 主题名已存在且未传 force，`{conflict: true, existingName:"..."}` |
| 500 | 服务内部错误 |

### 2. 设置 UI（`AppearancePage.vue` 新增区块）

在外观页底部新增"在线编辑"卡片，包含：

- **开关**：`开启在线连接`，控制 HTTP server 启停
- **状态行**：开关打开后显示 `🟢 监听中 localhost:29731`；端口启动失败显示具体错误
- **复制按钮**：一键复制连接地址到剪贴板
- **说明文字**：`在 Web 编辑器的连接设置中填入此地址，即可一键推送主题`

开关状态持久化到设置（`ui.themeLiveServer.enabled`、`ui.themeLiveServer.port`），下次打开设置页时自动恢复。

### 3. Web 编辑器连接面板（WindInputThemeEditor，API 契约）

编辑器侧需实现：

- 连接设置面板：输入框（默认 `http://localhost:29731`）、连接/断开按钮
- 连接后：调用 `GET /api/themes` 展示本地主题列表
- 选择主题：调用 `GET /api/theme/:slug` 加载 YAML 到编辑器
- 推送按钮：调用 `POST /api/theme/push`
  - 成功：提示"已推送，输入法正在刷新…"
  - 409 冲突：弹出确认框，确认后以 `force:true` 重推
  - 400/500：展示错误信息

---

## 数据流

### 拉取 + 编辑 + 推送

```
1. 用户在设置页开启"在线连接"
2. wind_setting 启动 HTTP server，展示监听地址
3. 用户在编辑器填写地址，点击"连接"
4. 编辑器 GET /api/themes  →  展示本地主题列表
5. 用户选择主题，编辑器 GET /api/theme/:slug  →  YAML 加载进编辑器
6. 用户修改完成，点击"推送到本地"
7. 编辑器 POST /api/theme/push {yaml, force:false}
8. wind_setting ImportThemeFromText()  →  校验+写入用户主题目录
9. wind_setting 通过现有 RPC 触发 wind_input 热重载
10. 返回 {imported:true, reloaded:true}  →  编辑器提示成功
```

### 端口冲突处理

```
尝试 29731 → 失败 → 尝试 29732 → 失败 → 尝试 29733 → 失败 → 返回错误
成功绑定后记录实际端口，设置 UI 展示正确地址
```

---

## 错误处理汇总

| 场景 | 处理 |
|------|------|
| 三个端口全被占用 | 启动失败，UI 提示"端口 29731-29733 均被占用，请手动指定端口" |
| wind_input 未运行 | 主题写入成功，reloaded:false，编辑器提示"主题已保存，待输入法启动后生效" |
| YAML schema 校验失败 | 400，编辑器展示具体字段错误 |
| 主题名冲突 | 409，编辑器二次确认后 force:true 覆盖 |
| 设置页已关闭 | 编辑器 fetch 超时，提示"无法连接，请确认输入法设置页已开启在线连接" |

---

## 配置项

在现有配置结构中新增（`ui.themeLiveServer`）：

```yaml
ui:
  themeLiveServer:
    enabled: false       # 是否开启（持久化开关状态）
    port: 29731          # 起始端口，冲突时自动 +1 最多尝试 3 次
```

---

## 实现范围

**本仓库（WindInput）需要修改的文件：**

| 文件 | 变更类型 | 说明 |
|------|----------|------|
| `wind_setting/theme_server.go` | 新建 | HTTP server + 3 个端点实现 |
| `wind_setting/app.go` | 修改 | 注册 ThemeServer，绑定生命周期 |
| `wind_setting/frontend/src/pages/AppearancePage.vue` | 修改 | 新增"在线编辑"卡片 UI |
| `wind_setting/frontend/src/api/wails.ts` | 修改 | 新增 `startThemeServer` / `stopThemeServer` 绑定 |
| `wind_input/pkg/config/schema.go`（或同等位置） | 修改 | 新增 `ui.themeLiveServer` 配置字段 |

**WindInputThemeEditor 仓库：** 按上述 API 契约独立实现连接面板，不在本文档范围内。

---

## 测试要点

- 端口自动递增：模拟 29731/29732 被占用，验证绑定到 29733
- 推送合法 YAML：验证主题写入 + 热重载触发
- 推送非法 YAML：验证 400 响应及错误信息
- 冲突覆盖：同名主题 force:false 返回 409，force:true 成功覆盖
- wind_input 未运行：验证 imported:true, reloaded:false 的降级路径
- CORS：验证允许列表内的 Origin 正常，列表外的 Origin 被拒绝
