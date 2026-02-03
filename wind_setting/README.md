# wind_setting - WindInput 设置工具

基于 Wails 框架的 WindInput 输入法图形化设置界面。

## 技术栈

- **后端**: Go + Wails v2
- **前端**: Vue 3 + TypeScript + Vite

## 功能

- 输入法配置管理（拼音/五笔切换、快捷键设置等）
- 词库管理
- 实时预览配置效果
- 与 wind_input 服务通信

## 项目结构

```
wind_setting/
├── app.go                   # Wails Go 后端主文件
├── internal/
│   ├── editor/              # 编辑器模块
│   └── filesync/            # 文件同步模块
├── frontend/                # Vue 3 前端
│   ├── src/
│   │   ├── api/             # API 接口
│   │   ├── components/      # Vue 组件
│   │   ├── App.vue          # 主应用组件
│   │   └── main.ts          # 入口文件
│   ├── index.html
│   └── package.json
├── build/
│   └── windows/
│       └── installer/       # NSIS 安装脚本
└── wails.json               # Wails 配置
```

## 开发

### 环境要求

- Go 1.21+
- Node.js 18+
- pnpm
- Wails CLI v2

### 安装 Wails

```bash
go install github.com/wailsapp/wails/v2/cmd/wails@latest
```

### 开发模式

```bash
wails dev
```

这将启动开发服务器，支持前端热重载。

### 构建

```bash
wails build
```

生成的可执行文件位于 `build/bin/wind_setting.exe`。

## 配置文件

设置工具读写的配置文件位于：
- 配置: `%APPDATA%\WindInput\config.yaml`
- 状态: `%APPDATA%\WindInput\state.yaml`

## 与输入服务通信

### 架构设计

设置工具采用**直接文件修改 + 管道通知**的架构：

```
┌─────────────────────────────────────────────────────────────────┐
│                    wind_setting                                  │
│  1. 读写配置文件 ──────────► %APPDATA%\WindInput\config.yaml    │
│  2. 发送重载命令 ──────────► \\.\pipe\wind_input_control        │
└─────────────────────────────────────────────────────────────────┘
                                        │
                                        ▼
                              ┌─────────────────┐
                              │  wind_input.exe │
                              │  重新加载配置   │
                              └─────────────────┘
```

### 控制管道

**管道名称**: `\\.\pipe\wind_input_control`

**支持的命令**:

| 命令 | 说明 |
|------|------|
| `PING` | 心跳检测 |
| `RELOAD_CONFIG` | 重新加载配置文件 |
| `RELOAD_PHRASES` | 重新加载短语定义 |
| `RELOAD_SHADOW` | 重新加载 Shadow 规则 |
| `RELOAD_USERDICT` | 重新加载用户词库 |
| `RELOAD_ALL` | 重新加载所有 |
| `GET_STATUS` | 获取服务状态 |

### 工作流程

1. **修改配置**: 直接写入 `config.yaml`
2. **通知重载**: 发送 `RELOAD_CONFIG` 到控制管道
3. **实时生效**: 服务重新加载配置并应用

### 使用控制客户端

```go
import "github.com/huanfeng/wind_input/pkg/control"

client := control.NewClient()
defer client.Close()

// 发送重载命令
resp, err := client.Send(control.CmdReloadConfig)
if err != nil || !resp.IsOk() {
    // 处理错误
}
```
