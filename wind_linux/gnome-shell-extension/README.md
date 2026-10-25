# windinput-caret

GNOME Wayland 没有 input-method-v2 时，dbus / ibus / fcitx4 报给 Fcitx5 的光标矩形往往是窗口相对坐标。这个扩展在 Shell 里把焦点窗口的相对光标换成 stage 坐标，经会话总线 `org.windinput.Caret` 交给清风的 Fcitx5 addon。addon 只在 `XDG_CURRENT_DESKTOP` 的冒号分段里含有 `GNOME` 时才会问它；KDE、treeland 不走这条路。

## 能在哪些 GNOME Shell 上加载

`extension.js` 是 ESM（`import` / `export default class`），这种扩展格式从 **GNOME Shell 45** 才有。`import Mtk from 'gi://Mtk'` 同样从 45 就有：gnome-shell 45.0 自己的 `js/ui/windowManager.js` 就是这么写的，[移植说明](https://gjs.guide/extensions/upgrading/gnome-shell-45.html) 也要求用 `Mtk.Rectangle` 换掉 `Meta.Rectangle`。

本扩展还调用 `Meta.Window.protocol_to_stage_rect`。这个方法从 **mutter 47.0** 起出现在 `src/meta/window.h`，46.0–46.7 的头文件里没有，45 更没有。所以 `metadata.json` 的 `shell-version` 只写 **47、48、49、50**。45 / 46 上即使改元数据强行启用，一调用就会因没有该方法失败。

Shell 51 的移植说明没有改到这里用的 import、`Mtk.Rectangle`、D-Bus 导出；mutter 51.0 仍有 `protocol_to_stage_rect` 与 `get_compositor_private`。尚未有人在 51 上启用过本扩展，元数据里先不写 51。

**不要把 Ubuntu 22.04 / GNOME Shell 42.9 当成这个扩展的实测环境。** 2026-10-10 在那台机器上看到的是「还没做扩展」时的光标（gedit 报窗口相对矩形，候选窗不跟窗口走）。Shell 42 不能加载 ESM 扩展。Ubuntu 24.04 的 Shell 是 46，同样不在 `shell-version` 里。上面的版本范围是对照上游标签核对 API 是否存在，**不是**清风在这些 Shell 上对过光标落点。分数缩放（输出缩放与 `Xft.dpi` 不一致）也还没有在真实会话里确认，需要作者补测。

## 安装

构建 addon 且同时找到 Wayland 与 `gio-2.0` 时，`cmake --install` 会把本目录装到

`${CMAKE_INSTALL_DATADIR}/gnome-shell/extensions/windinput-caret@windinput/`

（前缀 `/usr` 时就是 `/usr/share/gnome-shell/extensions/windinput-caret@windinput/`）。`.deb` 走的就是这条安装规则。

不装包、只在当前用户启用：

```bash
mkdir -p ~/.local/share/gnome-shell/extensions
cp -a wind_linux/gnome-shell-extension/windinput-caret@windinput \
    ~/.local/share/gnome-shell/extensions/
gnome-extensions enable windinput-caret@windinput
```

Wayland 会话不能用 Alt+F2 再输入 `r` 重启 Shell，启用后注销再登录。用 `gnome-extensions info windinput-caret@windinput` 看是否已启用。Shell 版本不在 `shell-version` 里时，启用会被拒绝，这是预期行为。

扩展没启用时 addon 退回不加重窗口原点的折中（最大化窗口大致可用，浮动窗口会偏），并且几秒内不会反复同步问总线。
