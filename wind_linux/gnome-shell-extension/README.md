# windinput-caret

GNOME Wayland 没有 input-method-v2 时，dbus / ibus / fcitx4 报给 Fcitx5 的光标矩形往往是窗口相对坐标。这个扩展在 Shell 里把焦点窗口的相对光标换成 stage 坐标，经会话总线 `org.windinput.Caret` 交给清风的 Fcitx5 addon。addon 只在 `XDG_CURRENT_DESKTOP` 的冒号分段里含有 `GNOME` 时才会问它；KDE、treeland 不走这条路。

## 能在哪些 GNOME Shell 上加载

有两份，导出同一个 D-Bus 接口，各自的 `shell-version` 只让对应的 Shell 启用：

| 目录（uuid） | 扩展格式 | Shell | 相对光标 → stage |
|---|---|---|---|
| `windinput-caret@windinput` | ESM（`import` / `export default class`） | 45–51 | 有 `Meta.Window.protocol_to_stage_rect`（mutter 47+）就用它，没有（45/46）就用窗口 actor 左上 + 相对坐标 |
| `windinput-caret-legacy@windinput` | 旧格式（`imports.gi` + `init()`） | 42–44 | 窗口 actor 左上 + 相对坐标 |

「窗口 actor 左上 + 相对坐标 × shellScale / 客户端 sf」是 kimpanel 扩展在 mutter 47 之前的做法：GTK3/GTK4/Qt
报的相对矩形以窗口 buffer 左上为原点、且已按客户端 sf 预乘。addon 传给扩展的除数在 sf>1 时一律是客户端 sf
（`spotScaleForGnomeExt`）。

**实测**：Ubuntu 22.04 / GNOME Shell 42.9 Wayland、显示器 150%（`scale-monitor-framebuffer`，Xwayland 根
960×600、`Xft.dpi` 96），legacy 扩展 + gedit（GTK3 报 sf=2）：最大化与浮动窗口挪到不同位置，候选窗都贴着
光标。47–51 上 Wayland 原生 Qt 应用（微信 4.1.13）在 100% 下确认过跟随光标；45/46 的回落分支、47+ 的分数
缩放、多显示器仍未在真机上对过。

Shell 51 的移植说明没有改到这里用的 import、`Mtk.Rectangle`、D-Bus 导出；mutter 51.0 仍有
`protocol_to_stage_rect` 与 `get_compositor_private`。

## 安装

构建 addon 且同时找到 Wayland 与 `gio-2.0` 时，`cmake --install` 会把本目录装到

`${CMAKE_INSTALL_DATADIR}/gnome-shell/extensions/windinput-caret@windinput/`

（前缀 `/usr` 时就是 `/usr/share/gnome-shell/extensions/windinput-caret@windinput/`）。`.deb` 走的就是这条安装规则。

不装包、只在当前用户启用：

```bash
mkdir -p ~/.local/share/gnome-shell/extensions
cp -a wind_linux/gnome-shell-extension/windinput-caret@windinput \
    wind_linux/gnome-shell-extension/windinput-caret-legacy@windinput \
    ~/.local/share/gnome-shell/extensions/
# Shell 45 及以上：
gnome-extensions enable windinput-caret@windinput
# Shell 42–44：
gnome-extensions enable windinput-caret-legacy@windinput
```

Shell 只在登录时扫描扩展目录：刚装上的扩展 `gnome-extensions enable` 会报「不存在」，要先注销重登一次再启用（或先把 uuid 加进 `org.gnome.shell enabled-extensions`，重登后自动加载）。Wayland 会话不能用 Alt+F2 再输入 `r` 重启 Shell。用 `gnome-extensions info windinput-caret@windinput` 看是否已启用。Shell 版本不在 `shell-version` 里时，启用会被拒绝，这是预期行为。

扩展没启用时 addon 退回不加重窗口原点的折中（最大化窗口大致可用，浮动窗口会偏），并且几秒内不会反复同步问总线。
