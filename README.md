<p align="center">
  <img src="pic/logo_fix.png" alt="清风输入法" width="128">
</p>

<h1 align="center">清风输入法 (WindInput)</h1>

<p align="center">
  轻量、快速、可定制的开源中文输入法
</p>

<p align="center">
  <img src="https://img.shields.io/github/v/release/huanfeng/WindInput?include_prereleases&label=version&color=blue" alt="Version">
  <img src="https://img.shields.io/badge/platform-Windows%2010%2F11-brightgreen" alt="Platform">
  <img src="https://img.shields.io/badge/macOS-12%2B%20alpha-orange" alt="macOS alpha">
  <img src="https://img.shields.io/badge/license-MIT-green" alt="License">
</p>

> **⚠️ 早期开发阶段**
>
> * 本项目目前处于 alpha 阶段，功能和配置格式可能随版本更新发生变化。\
> * 升级后如遇到异常，请尝试删除配置目录 `%APPDATA%\WindInput\` 以恢复默认配置。

> **⚠️ 已知问题**
>
> * 输入法天然需要更高权限，当前没有数字签名，所以安装时可能会被 Windows 安全中心拦截。\
> * 本输入法采用独立窗口渲染模式，在没有数字签名的情况下，无法申请更高的显示层级，所以无法显示在开始菜单之上，目前已通过跨进程的宿主渲染方式优化，但在开始菜单上的候选输入框不支持鼠标操作。

## 特性

- **专为五笔设计** — 支持五笔 86、五笔拼音混输，同时提供全拼和双拼输入
- **智能候选** — 精准匹配，快速上屏
- **高 DPI 适配** — 完美支持高分辨率和多显示器环境，界面清晰锐利
- **亮暗主题** — 支持主题的亮色和暗色模式，并且可以随系统自动切换
- **状态提示** — 输入光标处会提示当前的中英文、标点及输入方案状态
- **方案驱动** — 通过 YAML 方案文件灵活定义输入行为
- **图形设置** — 内置设置工具，所有配置可视化调整，修改即时生效
- **轻量运行** — 资源占用低，启动迅速

## 安装

### 使用安装包（推荐）

从 [Releases](../../releases) 页面下载最新的安装包（`WindInput-x.x.x-Setup.exe`），双击运行即可。

安装完成后，按 `Win + Space` 或 `Ctrl + Shift` 切换到清风输入法。

### 便携版

从 [Releases](../../releases) 页面下载 `WindInput-x.x.x-Portable.zip`，解压到任意目录后运行 `wind_portable.exe` 即可使用。

便携版数据保存在程序所在目录，适合 U 盘携带或多环境使用。更新时下载新版 ZIP 覆盖解压即可。

### macOS

从 [Releases](../../releases) 页面下载 `WindInput-x.x.x-macOS.pkg`，双击运行安装。安装包内含**输入法、后台服务、设置程序**三件套，为 universal 构建（同时支持 Apple Silicon 与 Intel）。

安装后到 **系统设置 → 键盘 → 文本输入 → 输入法** 添加并切换到「清风输入法」（也可用 `⌃Space` 或菜单栏输入菜单切换）。设置程序为 `~/Applications/清风输入法设置.app`，也可从输入法菜单的「设置…」打开。卸载：双击 `~/Applications/卸载清风输入法.app`。

> 需要 macOS 12 及以上。当前版本未做苹果公证，首次安装/启用可能需要在「系统设置 → 隐私与安全性」中点「仍要打开」放行；macOS 26 (Tahoe) 对未公证输入法限制更强。重新安装前建议先注销或重启，以清除系统的输入法注册缓存。

### 手动构建安装

如需从源码构建，请参阅 [开发文档](docs/DEVELOPMENT.md)。

## 使用方法

1. 使用 `Win + Space` 或 `Ctrl + Shift` 切换到**清风输入法**
2. 输入拼音或五笔编码，候选窗口自动显示
3. 数字键 `1-9` 选择候选词，`空格` 选择第一个
4. `Shift` 切换中英文模式
5. `Esc` 取消当前输入
6. `Enter` 输出原始编码

## 配置

配置文件位于 `%APPDATA%\WindInput\config.yaml`，也可通过设置工具修改：

```yaml
schema:
  active: "wubi86"            # 当前输入方案：wubi86 / wubi86_pinyin

hotkeys:
  toggle_mode_keys: [lshift, rshift]   # 中英切换键

ui:
  font_size: 18             # 候选窗字体大小
  candidates_per_page: 7    # 每页候选数量
```

完整配置项请参阅设置工具中的说明。

## 技术概览

清风输入法采用 C++/Go 混合架构：

| 组件 | 技术 | 职责 |
|------|------|------|
| `wind_tsf` | C++ | Windows TSF 框架接口，键盘事件捕获 |
| `wind_input` | Go | 输入引擎、候选词管理、UI 渲染（**跨平台**：Win 全功能 + macOS 服务端可运行） |
| `wind_setting` | Go + Vue 3 | 图形化设置工具（Win / macOS） |
| `wind_macos` | Swift | macOS IMKit `.app` 输入法客户端，对位 `wind_tsf` |

架构详情和开发指南请参阅 [开发文档](docs/DEVELOPMENT.md)。

### macOS 版本 (alpha)

macOS 端采用 **IMKit `.app` 输入法客户端 + Go 后台服务** 的双进程模型，输入、候选、上屏、设置界面均已打通，以单个 `.pkg` 分发上述三件套（universal，支持 Apple Silicon 与 Intel）。安装方式见上文 [安装 → macOS](#macos)。

仍处于 alpha：未做苹果公证，新系统 (macOS 26 Tahoe) 对未公证输入法限制更强；功能与 Windows 版存在差异。

macOS 开发者入口文档：

| 文档 | 用途 |
|------|------|
| [`docs/design/macos-port.md`](docs/design/macos-port.md) | 整体架构 (IMKit + Go 双进程模型) 与设计决策 |
| [`docs/design/macos-imkit-plan.md`](docs/design/macos-imkit-plan.md) | **PR-A 工程详细开发计划** (目录结构 / 类骨架 / 6 个里程碑 / 验证步骤) |
| [`docs/wire-protocol-reference.md`](docs/wire-protocol-reference.md) | bridge + uicmd 协议速查 (Swift 解码器实现参考) |
| [`docs/macos-build.md`](docs/macos-build.md) | macOS 上构建/调试 Go 服务的实用指南 |

参考实现 (鼠须管 Squirrel): <https://github.com/rime/squirrel>

## 参与贡献

欢迎贡献代码、报告 Bug 或提出建议！请阅读 [贡献指南](CONTRIBUTING.md) 了解详情。

> 注意：首次提交 PR 需要签署 [贡献者许可协议 (CLA)](CLA.md)。

## 第三方资源

本项目使用了以下第三方数据资源：

| 资源 | 用途 | 许可证 |
|------|------|--------|
| [白霜拼音 (rime-frost)](https://github.com/gaboolic/rime-frost) | 拼音词库数据源 | GPL-3.0 |
| [极点五笔 for Rime](https://github.com/KyleBing/rime-wubi86-jidian) | 五笔 86 码表数据源 | Apache-2.0 |
| [pinyin-data](https://github.com/mozillazg/pinyin-data) | 汉字拼音注音数据（悬停提示） | MIT |
| 腾讯词向量 | 词频权重参考 | — |
| 五笔86拆字数据库 (`wubi86_chaizi.txt`) | 五笔字根拆字提示 | 来源不详，未附版权说明 |
| 黑体字根字体 (`HeiTiZiGen.ttf`) | 拆字提示中的 PUA 字根字符渲染 | 来源不详，未附版权说明 |

详细的第三方资源声明请参阅 [NOTICE.md](NOTICE.md)。

## 许可证

本项目源代码采用 [MIT 许可证](LICENSE)。

词库数据来源于第三方项目，适用各自的许可证条款，详见 [NOTICE.md](NOTICE.md)。

## 交流与反馈

- **QQ 交流群**: [1085293418](https://qm.qq.com/q/u2A8FfafIs) — 清风输入法官方交流群
- **GitHub Issues**: [问题反馈](../../issues) — 报告 Bug 或提出建议

## 相关链接

- [更新日志](CHANGELOG.md)
- [开发文档](docs/DEVELOPMENT.md)
- [贡献指南](CONTRIBUTING.md)
- [第三方声明](NOTICE.md)
