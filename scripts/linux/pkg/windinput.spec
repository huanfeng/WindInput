# 清风输入法 rpm：把 stage-linux.sh 组装好的安装树原样打包（不在 rpm 里重新编译）。
# 构建在 Ubuntu 22.04 基线上完成（glibc 2.35），这里只负责元数据、依赖与 scriptlet；
# 调用方 build-rpm.sh 经 --define 传入：
#   wind_version  rpm Version（预发布后缀的 - 已换成 ~）
#   stage_dir     安装树（stage-linux.sh 的 STAGE_DIR）
#   src_dir       仓库根（取 LICENSE / NOTICE.md）
# 同一个 rpm 面向 Fedora 与 openSUSE：发行版专属的差异（数据库刷新等）放在 scriptlet 里按命令是否存在
# 运行时判断，而不是用 %if 宏——宏在构建机（Fedora）上就定死了，装到 openSUSE 时已经没有机会再分支。

# 二进制已是成品：不再抽 debuginfo、不 strip、不跑 brp 脚本（会改写已核对过的 ELF）。
%global debug_package %{nil}
%global __os_install_post %{nil}
# xz 压缩：词库占大头，xz 体积最小，且 Leap 15.x 的 rpm 4.14 也认。
%global _binary_payload w6.xzdio
%global source_date_epoch_from_changelog 0

Name:           windinput
Version:        %{wind_version}
Release:        1
Summary:        清风输入法 (WindInput) —— Fcitx5 输入法引擎
License:        GPL-3.0-or-later
URL:            https://windinput.com

# 基线与 deb 一致：addon 按 Fcitx5 5.0.14 头文件编译。链接期依赖（libc / libstdc++ / libFcitx5*.so / libxcb*）
# 由 rpm 按 ELF 自动算，不在这里手写。
Requires:       fcitx5 >= 5.0.14
# 运行时 dlopen 的库，ELF 里没有 NEEDED，自动依赖看不到
Requires:       fontconfig
# scriptlet 用 pkill：写成文件依赖，Fedora（procps-ng）与 openSUSE（procps）包名不同
Requires(post):   /usr/bin/pkill
Requires(postun): /usr/bin/pkill
# 弱依赖：名字在某个发行版不存在时被忽略，所以两家的名字都列
Recommends:     fcitx5-gtk
Recommends:     fcitx5-qt
Recommends:     xclip
Recommends:     wl-clipboard
Recommends:     xdg-utils
Recommends:     xdg-desktop-portal
Recommends:     zenity
Recommends:     google-noto-sans-cjk-fonts
Recommends:     noto-sans-cjk-fonts
Recommends:     google-noto-emoji-color-fonts
Recommends:     noto-coloremoji-fonts

%description
清风输入法的 Linux 版（测试版）：Rust 服务负责输入逻辑与候选窗渲染，
Fcitx5 addon 负责与应用对接，另带图形设置程序（应用菜单「清风输入法设置」，
或在输入时按 Ctrl+Shift+]）。安装后在 Fcitx5 配置的输入法列表里添加「清风输入法」即可使用。

%prep
%build

%install
cp -a %{stage_dir}/. %{buildroot}/
# stage 目录可能是 build-deb.sh 用过的（带 DEBIAN/ 元数据），不属于 rpm
rm -rf %{buildroot}/DEBIAN
# deb 的 addon 在 Debian 多架构目录（usr/lib/<multiarch>/fcitx5），rpm 系是 %{_libdir}/fcitx5
# （x86_64 / aarch64 都是 lib64）。
addon="$(find %{buildroot}/usr/lib -name libwindinput.so -path '*/fcitx5/*')"
test -n "$addon"
install -d %{buildroot}%{_libdir}/fcitx5
if [ "$(dirname "$addon")" != "%{buildroot}%{_libdir}/fcitx5" ]; then
    mv "$addon" %{buildroot}%{_libdir}/fcitx5/libwindinput.so
    rmdir -p --ignore-fail-on-non-empty "$(dirname "$addon")"
fi
install -Dm644 %{src_dir}/LICENSE   %{buildroot}%{_licensedir}/%{name}/LICENSE
install -Dm644 %{src_dir}/NOTICE.md %{buildroot}%{_licensedir}/%{name}/NOTICE.md
# 文件清单：全部文件逐个列，目录只认自己私有的那几个（共享目录 /usr/share/icons 等归系统包所有）。
( cd %{buildroot}
  find . \( -type f -o -type l \) | sed 's|^\.||'
  find ./usr/lib/windinput ./usr/share/doc/windinput ./usr/share/licenses/%{name} \
       ./usr/share/gnome-shell/extensions/windinput-* -type d 2>/dev/null | sed 's|^\.|%dir |'
) > %{_builddir}/filelist
# 防空包：安装树不完整（文件数异常少）时直接失败
test "$(wc -l < %{_builddir}/filelist)" -gt 100

%files -f %{_builddir}/filelist
%defattr(-,root,root,-)

%post
# 升级时旧服务还在跑旧二进制：结束它，addon 在下次连不上时会自动拉起新版。
# 按整条命令行精确匹配包里那个路径：同名开发构建、`wind_input restart` 这类 CLI 调用不受影响。
/usr/bin/pkill -x -f '/usr/lib/windinput/wind_input( --restarted)?' >/dev/null 2>&1 || :
# Fedora 靠 file trigger 刷新这些数据库；openSUSE 没有，装完要自己刷（命令不存在就跳过）。
command -v update-mime-database >/dev/null 2>&1 && update-mime-database /usr/share/mime >/dev/null 2>&1 || :
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q /usr/share/applications >/dev/null 2>&1 || :
command -v gtk-update-icon-cache >/dev/null 2>&1 && [ -f /usr/share/icons/hicolor/icon-theme.cache ] && gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor >/dev/null 2>&1 || :
echo "清风输入法已安装。在 Fcitx5 配置的输入法列表里添加「清风输入法」即可使用（首次安装需注销并重新登录；升级只需重启 fcitx5：fcitx5 -rd）。"
# GNOME 的 Wayland 会话没有输入法窗口定位接口，候选窗要靠随包的 Shell 扩展才跟得准光标，
# 而扩展装完默认不启用（系统包碰不到用户的桌面配置）——这里只能提示。仅本机有 gnome-shell 时才说。
if command -v gnome-shell >/dev/null 2>&1; then
    echo ""
    echo "【GNOME 用户请注意】若使用 Wayland 会话（Ubuntu 默认），候选窗需要启用随包的 GNOME 扩展才会跟随光标："
    echo "  1. 注销并重新登录（GNOME 只在登录时扫描新装的扩展）；"
    echo "  2. 打开「清风输入法设置」，点窗口顶部黄色提示条里的「启用」；"
    echo "     或在终端执行（GNOME 45 及以上）：gnome-extensions enable windinput-caret@windinput"
    echo "     （GNOME 42–44：gnome-extensions enable windinput-caret-legacy@windinput）；"
    echo "  3. 再注销重登一次。"
    echo "  X11 会话（Ubuntu on Xorg）不需要这一步。"
fi

%postun
# 只在真卸载时结束服务（升级时 $1 = 1，新版 %post 已处理）。放 postun 而不是 preun：文件删掉之后再结束，
# addon（仍在已运行的 fcitx5 里）想重新拉起时程序已不在，不会在 preun 与删文件之间又起一个孤儿。
if [ "$1" -eq 0 ]; then
    /usr/bin/pkill -x -f '/usr/lib/windinput/wind_input( --restarted)?' >/dev/null 2>&1 || :
fi
command -v update-mime-database >/dev/null 2>&1 && update-mime-database /usr/share/mime >/dev/null 2>&1 || :
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q /usr/share/applications >/dev/null 2>&1 || :
command -v gtk-update-icon-cache >/dev/null 2>&1 && [ -f /usr/share/icons/hicolor/icon-theme.cache ] && gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor >/dev/null 2>&1 || :
