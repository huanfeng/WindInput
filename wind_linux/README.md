# wind_linux — 清风输入法 Fcitx5 前端

清风输入法在 Linux 上以 [Fcitx5](https://fcitx-im.org) 输入法引擎的形式工作：本目录是 Fcitx5
addon（`libwindinput.so`），负责把按键交给清风输入法服务（`wind_input`，Rust）、把上屏与预编辑
写回应用。输入逻辑、词库、候选全部在服务里。

## 构建

依赖：Fcitx5 5.x 开发包（`libfcitx5core-dev` `libfcitx5utils-dev` `libfcitx5config-dev`）、
`libxcb1-dev` `libxcb-shape0-dev` `libxcb-randr0-dev`（候选窗 / 工具栏）、CMake ≥ 3.16、C++17 编译器。较新的 Fcitx5 头文件（如 Arch 的 5.1.23）
要 C++20，配置时会自动探测并把 addon 升到 C++20，此时需要支持 C++20 的编译器；也可用 `-DCMAKE_CXX_STANDARD=20` 指定。

```bash
cmake -S wind_linux -B wind_linux/build/cmake -G Ninja
cmake --build wind_linux/build/cmake
cmake --install wind_linux/build/cmake --prefix /usr     # 需要 root；或装到 ~/.local 并设 FCITX_ADDON_DIRS
```

服务：

```bash
cd wind_input && cargo build --release -p wind_service --features linux-host
```

没有 root 的开发机用 `scripts/linux/bootstrap-sdk.sh` 把 Fcitx5 SDK 解到用户目录。

## 安装包（Ubuntu/Debian）

```bash
scripts/linux/package-deb.sh          # 在 22.04 容器里构建，产物 dist/linux/windinput_<版本>_amd64.deb
sudo apt install ./windinput_*.deb     # 会带上 fcitx5 及 GTK/Qt 前端
```

装完不需要任何额外脚本，按 Fcitx5 的标准用法：

1. 系统的输入法框架用 Fcitx5（Deepin 默认就是；Ubuntu 桌面用系统的「语言支持」把键盘输入法系统设为
   Fcitx5，或 `im-config -n fcitx5`——这是所有 Fcitx5 输入法共同的前提，不是本包的额外步骤），
   注销并重新登录；
2. 打开「Fcitx5 配置」（fcitx5-configtool），把「清风输入法」加进输入法列表；
3. Shift 单击切中英开箱可用（addon 自己接管，不必改 Fcitx5 的全局配置）。

升级只需 `fcitx5 -rd`（或重新登录）。

包里带图形设置程序（`/usr/lib/windinput/wind_setting`）：应用菜单「清风输入法设置」，或输入时按
`Ctrl+Shift+]`。构建时需要兄弟仓库 `../wind-setting` 与 `../wind-ui-rust`（`SETTING_REPO` 可指定）。
设置程序的文件对话框依赖 xdg-desktop-portal，弹不出时退 zenity 或 kdialog（包已 Recommends）。

必须在 22.04 上构建：更新的发行版编出的产物带 GLIBC_2.38+ 符号和更新的 fcitx5 ABI，22.04 上加载不了。
服务由 addon 在连不上时自动拉起（`/usr/lib/windinput/wind_input`，可用 `WIND_INPUT_SERVICE` 覆盖），
服务自己持 flock 单例。

## 已知限制

- **KDE Plasma（Wayland）**：要在「系统设置 → 键盘 → 虚拟键盘」里选「Fcitx 5」并重新登录——KWin
  只把输入法协议（input-method-v1）交给它自己拉起的 Fcitx5，否则 Wayland 原生应用里没有候选窗。
  并按 Fcitx5 的建议**不要**设 `QT_IM_MODULE` / `GTK_IM_MODULE`：设了的 Wayland 原生应用走 D-Bus、
  报的是窗口内坐标，候选窗会落到屏幕左上角。
- Fcitx5 默认每个应用各记各的输入法状态，新开的应用从键盘布局起步；想全局一致，在 Fcitx5 配置的
  「全局选项」里把「共享输入状态」设为「所有」（Deepin 出厂即如此）。

## 使用

1. 服务由 addon 在连不上时自动拉起；手动启动 `wind_input`（exe 同目录要有 `data/` 词库目录）仅用于开发。
   socket 在 `$XDG_RUNTIME_DIR/WindInput/`。
2. 在 Fcitx5 配置里把「清风输入法」加入输入法组。
3. 设置：应用菜单或 Fcitx5 托盘菜单里的「清风输入法设置」、Fcitx5 输入法配置里清风的「配置」，
   或在输入时按 `Ctrl+Shift+]`（开发时设置程序路径可用
   `WIND_INPUT_SETTING` 覆盖，默认 `/usr/lib/windinput/wind_setting`）。
   主菜单：Fcitx5 托盘菜单里的「清风输入法菜单」（弹在指针处），或组字时右键候选窗。
4. Shift 单击切中英：Fcitx5 出厂把左 Shift 配成「临时在当前和第一个输入法之间切换」
   （`AltTriggerKeys`），会在引擎之前截走。addon 在更早的阶段把 Shift 单击接过来
   （`WindEngine::claimShiftTap`），用户不必改配置；清空了该项或当前不是清风时不插手。

## 测试

```bash
make -C wind_linux test      # 单测（不需要 Fcitx5）
scripts/linux/e2e.sh         # 端到端：真服务 + 真 fcitx5 + DBus 模拟应用打字
```

开发约定与架构说明见 [AGENTS.md](AGENTS.md)。
