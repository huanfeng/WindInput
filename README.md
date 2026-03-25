<p align="center">
  <img src="pic/logo_fix.png" alt="清风输入法" width="128">
</p>

<h1 align="center">清风输入法 (WindInput)</h1>

<p align="center">
  轻量、快速、可定制的开源中文输入法
</p>

<p align="center">
  <img src="https://img.shields.io/badge/version-0.1.0--alpha-blue" alt="Version">
  <img src="https://img.shields.io/badge/platform-Windows%2010%2F11-brightgreen" alt="Platform">
  <img src="https://img.shields.io/badge/license-MIT-green" alt="License">
</p>

> **⚠️ 早期开发阶段**
>
> 本项目目前处于 alpha 阶段（v0.1.0-alpha），功能和配置格式可能随版本更新发生变化。
> 升级后如遇到异常，请尝试删除配置目录 `%APPDATA%\WindInput\` 以恢复默认配置。

## 特性

- **专为五笔设计** — 支持五笔 86、五笔拼音混输，同时提供全拼和双拼输入
- **智能候选** — 精准匹配，快速上屏
- **高 DPI 适配** — 完美支持高分辨率和多显示器环境，界面清晰锐利
- **快捷切换** — Shift/Ctrl/CapsLock 中英文切换可配置，Shift + 字母临时英文输入
- **方案驱动** — 通过 YAML 方案文件灵活定义输入行为
- **图形设置** — 内置设置工具，所有配置可视化调整，修改即时生效
- **轻量运行** — 资源占用低，启动迅速

## 安装

### 使用安装包（推荐）

从 [Releases](../../releases) 页面下载最新的安装包（`清风输入法-x.x.x-Setup.exe`），双击运行即可。

安装完成后，按 `Win + Space` 或 `Ctrl + Shift` 切换到清风输入法。

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
engine:
  type: pinyin              # pinyin / wubi

hotkeys:
  toggle_mode_keys: [lshift, rshift]   # 中英切换键

ui:
  font_size: 18             # 候选窗字体大小
  candidates_per_page: 9    # 每页候选数量
```

完整配置项请参阅设置工具中的说明。

## 技术概览

清风输入法采用 C++/Go 混合架构：

| 组件 | 技术 | 职责 |
|------|------|------|
| `wind_tsf` | C++ | Windows TSF 框架接口，键盘事件捕获 |
| `wind_input` | Go | 输入引擎、候选词管理、UI 渲染 |
| `wind_setting` | Go + Vue 3 | 图形化设置工具 |

架构详情和开发指南请参阅 [开发文档](docs/DEVELOPMENT.md)。

## 参与贡献

欢迎贡献代码、报告 Bug 或提出建议！请阅读 [贡献指南](CONTRIBUTING.md) 了解详情。

> 注意：首次提交 PR 需要签署 [贡献者许可协议 (CLA)](CLA.md)。

## 第三方资源

本项目的词库数据使用了以下开源项目：

| 资源 | 用途 | 许可证 |
|------|------|--------|
| [雾凇拼音 (rime-ice)](https://github.com/iDvel/rime-ice) | 拼音词库数据源 | GPL-3.0 |
| [极点五笔 for Rime](https://github.com/KyleBing/rime-wubi86-jidian) | 五笔 86 码表数据源 | Apache-2.0 |
| 腾讯词向量 | 词频权重参考 | — |

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
