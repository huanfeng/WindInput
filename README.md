<p align="center">
  <img src="pic/logo.png" alt="清风输入法" width="128">
</p>

<h1 align="center">清风输入法 (WindInput)</h1>

<p align="center">
  轻量、快速、可定制的开源中文输入法
</p>

<p align="center">
  <img src="https://img.shields.io/badge/platform-Windows%2010%2F11-brightgreen" alt="Platform">
  <img src="https://img.shields.io/badge/macOS-12%2B-blue" alt="macOS">
  <img src="https://img.shields.io/badge/Linux-Fcitx5%20%28preview%29-orange" alt="Linux">
  <img src="https://img.shields.io/badge/license-GPL--3.0-blue" alt="License">
</p>

<p align="center">
  <a href="https://windinput.com"><b>官网</b></a> ·
  <a href="https://windinput.com/download"><b>下载</b></a> ·
  <a href="https://windinput.com/docs"><b>使用文档</b></a> ·
  <a href="https://windinput.com/changelog"><b>更新日志</b></a>
</p>

<p align="center">
  <a href="https://hellogithub.com/repository/huanfeng/WindInput" target="_blank"><img src="https://api.hellogithub.com/v1/widgets/recommend.svg?rid=565ab2afe06642d3825156e0ec556428&claim_uid=3OseFoEribJzy5D&theme=neutral" alt="Featured｜HelloGitHub" style="width: 250px; height: 54px;" width="250" height="54" /></a>
  <a href="https://atomgit.com/huanfengf/WindInput" target="_blank"><img src="https://atomgit.com/huanfengf/WindInput/star/new_badge.svg" alt="AtomGit" height="54" /></a>
</p>

## 特性

- **专为五笔和码表输入方案设计** — 五笔 86、五笔拼音混输，同时提供全拼和双拼
- **方案驱动** — 通过方案文件灵活定义输入行为
- **图形设置** — 配套设置工具，配置可视化调整，修改即时生效
- **亮暗主题** — 支持亮色和暗色主题，可随系统自动切换
- **轻量运行** — Rust 实现，资源占用低，启动迅速
- **跨平台** — 同一套核心引擎，支持 Windows 10/11、macOS 12+ 与 Linux（Fcitx5，预览版）

## 安装

前往 [windinput.com/download](https://windinput.com/download) 下载对应平台的安装包：

- **Windows 10/11（64 位）** — 安装版或便携版，安装后按 `Win + Space` 切换到清风输入法
- **macOS 12+** — `.pkg` 通用安装包（Apple Silicon / Intel），安装后在
  「系统设置 → 键盘 → 文本输入 → 编辑」中添加清风输入法，再从输入法菜单切换
- **Linux（预览版）** — `.deb` 安装包（amd64 / arm64，Ubuntu 22.04+、Debian 系发行版），以
  Fcitx5 输入法引擎的形式工作；`sudo apt install ./WindInput-*.deb` 安装后，在「Fcitx5 配置」中
  把清风输入法加入输入法列表。系统输入法框架需为 Fcitx5，KDE Plasma 等桌面的额外设置与已知限制见
  [wind_linux/README.md](wind_linux/README.md)
- **Linux（Nix / NixOS）** — 仓库自带 flake：NixOS 把 `windinput` 包加进
  `i18n.inputMethod.fcitx5.addons`，其它发行版 `nix profile install github:huanfeng/WindInput#windinput`
  后把该包加进 Fcitx5 的 addon 目录。构建产物、用法与限制见
  [docs/design/nix-packaging.md](docs/design/nix-packaging.md)

Windows 版的程序文件与安装包已做数字签名；SmartScreen 需要下载量积累信誉，新版本发布初期
仍可能拦截，按提示「更多信息 → 仍要运行」继续即可。macOS 版尚未做苹果签名与公证，被
Gatekeeper 拦下时到「系统设置 → 隐私与安全性」放行。

## 文档

完整的使用说明、配置参考和常见问题都在文档站：**[windinput.com/docs](https://windinput.com/docs)**

## 仓库范围

本仓库包含清风输入法的核心部分：

| 组件 | 技术 | 职责 |
|------|------|------|
| `wind_input` | Rust | 核心服务：输入引擎、词库、候选管理、UI 渲染、IPC（跨平台） |
| `wind_tsf` | C++ | Windows TSF 输入法框架接口，键盘事件捕获 |
| `wind_macos` | Swift | macOS IMKit 输入法客户端 |
| `wind_linux` | C++ | Linux Fcitx5 addon（预览版，见 [wind_linux/README.md](wind_linux/README.md)） |
| `flake.nix` + `nix/` | Nix | Linux 端的 Nix 打包（服务 + addon + 设置程序 + 词库数据，见 [docs/design/nix-packaging.md](docs/design/nix-packaging.md)） |

发布版安装包还需要以下配套项目，它们在各自的独立仓库中维护：

| 仓库 | 技术 | 职责 | 开源状态 |
|------|------|------|----------|
| [wind-installer](https://github.com/huanfeng/wind-installer) | Rust | Windows 安装器与卸载器（清单驱动的通用打包器） | 已开源 |
| [wind-portable](https://github.com/huanfeng/wind-portable) | Rust | Windows 便携版（绿色版）启动器，免安装就地运行 | 已开源 |
| [wind-ui-rust](https://github.com/huanfeng/wind-ui-rust) | Rust | 跨平台轻量 GUI 库，安装器 / 便携版 / 设置程序的界面基础 | 已开源 |
| [wind-setting](https://github.com/huanfeng/wind-setting) | Rust | 图形设置程序 | 已开源 |

其中 wind-installer 与 wind-portable 只用于 Windows；macOS 的 `.pkg` 安装包由本仓库的
`scripts/mac/dev.sh pkg` 直接打包，Linux 的 `.deb` 由 `scripts/linux/package-deb.sh` 打包，均无需额外仓库。

这些配套项目都是可选的：与本仓库放在同级目录时构建脚本会一并构建，缺失则自动跳过，
核心输入法本身可独立构建和运行。完整成品请从[下载页](https://windinput.com/download)获取。

wind-installer 与 wind-portable 通过 crates.io 上发布的 `windui` 使用 GUI 库，无需本地检出
wind-ui-rust；只有 wind-setting 是 path 依赖（`windui = { path = "../wind-ui-rust" }`），
构建它时才需要把 wind-ui-rust 一并放到同级目录。

## 从源码构建

- **Windows**：Rust stable + Visual Studio 2022（C++ 桌面开发）+ CMake，运行 `.\scripts\dev.ps1`
- **macOS**：Rust stable + Xcode（Swift 5.9+），运行 `scripts/mac/dev.sh`
- **Linux（交叉编译 Windows 产物）**：`scripts/dev.sh`
- **Linux（Fcitx5 前端，预览版）**：构建、安装与已知限制见 [wind_linux/README.md](wind_linux/README.md)
- **Linux（Nix，任意发行版 / NixOS）**：`nix build .#windinput`；开发环境 `nix develop`。
  词库数据同样在 Nix 内构建（等价于 gen-data），细节见
  [docs/design/nix-packaging.md](docs/design/nix-packaging.md)

构建脚本会自动下载第三方词库数据并生成完整的数据目录。详细说明见
[贡献指南](CONTRIBUTING.md)。

## 参与贡献

欢迎贡献代码、报告 Bug 或提出建议！请阅读 [贡献指南](CONTRIBUTING.md)。

> 首次提交 PR 需要签署 [贡献者许可协议 (CLA)](CLA.md)。

## 致谢

清风输入法在设计与实现过程中参考了许多优秀的开源输入法项目，
它们的思路、文档与源码为本项目提供了重要的指引，在此一并致谢：

- **[RIME 中州韵输入法引擎](https://rime.im)**（[librime](https://github.com/rime/librime)）
  — 方案与词库格式、候选排序与整句解码模型的主要参考；其
  [小狼毫 Weasel](https://github.com/rime/weasel) 是 Windows TSF 集成语义的对照实现，
  [鼠须管 Squirrel](https://github.com/rime/squirrel) 是 macOS IMKit 架构的参考
- **[Fcitx5](https://github.com/fcitx/fcitx5)**（[libime](https://github.com/fcitx/libime)）
  — 拼音解码（不完整拼音、超长词惩罚）与用户词频模型的重要参考；
  fcitx5-macos 为 macOS 端的打包与输入源注册细节提供了对照
- **极点五笔** — 五笔输入的交互习惯与功能来源；
  五笔 86 码表数据来自其 Rime 移植版
  [rime-wubi86-jidian](https://github.com/KyleBing/rime-wubi86-jidian)
- **[白霜拼音 rime-frost](https://github.com/gaboolic/rime-frost)** — 拼音词库与语言模型的数据来源
- **[pinyin-data](https://github.com/mozillazg/pinyin-data)** — 汉字读音数据来源
- **[OpenCC](https://github.com/BYVoid/OpenCC)** — 简繁转换词典数据来源
- **[Windows Classic Samples](https://github.com/microsoft/Windows-classic-samples)** 与
  [TSF 官方文档](https://learn.microsoft.com/en-us/windows/win32/tsf/text-services-framework)
  — Windows 输入法框架实现参考

上述项目均为独立作品，本项目仅作实现参考，未复制其源代码；
数据来源部分适用各自的许可证条款，详见 [NOTICE.md](NOTICE.md)。

## 许可证

本项目源代码自 v0.127.0 起采用 [GNU 通用公共许可证第 3 版或更高版本](LICENSE)（GPL-3.0-or-later）。
v0.127.0 之前发布的版本仍适用其发布时的 MIT 许可证。

基于本项目修改并分发的作品须同样以 GPL-3.0 发布并提供完整源代码。

词库数据来源于
[白霜拼音](https://github.com/gaboolic/rime-frost)、[极点五笔](https://github.com/KyleBing/rime-wubi86-jidian)、
[pinyin-data](https://github.com/mozillazg/pinyin-data)、[OpenCC](https://github.com/BYVoid/OpenCC)
等第三方项目，适用各自的许可证条款，完整声明详见 [NOTICE.md](NOTICE.md)。

## 关于项目名称

GPL 许可证授予您使用、修改与分发源代码的权利，但不包括使用项目名称与标识的权利。

若您将本项目分支后公开发布，请更换项目名称与 logo，并注明为非官方分支，
以便用户区分软件的实际维护者。

本项目未注册商标，上述为约定与请求而非法律条款，
详见 [项目名称与标识使用约定](BRANDING.md)。

## 交流与反馈

- **QQ 交流群**：[1085293418](https://qm.qq.com/q/u2A8FfafIs)
- **GitHub Issues**：[问题反馈](../../issues)

## 相关项目

- [WindInput-Go](https://github.com/huanfeng/WindInput-Go) — 清风输入法的前身（Go 实现），本项目由其移植重写而来
