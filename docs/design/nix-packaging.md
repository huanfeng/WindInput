# Linux 端的 Nix 打包

本仓自带 `flake.nix` + `nix/`，把 Linux（Fcitx5）形态的清风输入法打成可复现的 Nix 包：
Rust 服务 + Fcitx5 addon + 图形设置程序 + 词库数据。Windows / macOS 形态不在 Nix 覆盖范围内
（前者要原生 MSVC 工具链，后者要 Xcode / IMKit）。

## 产物

| 属性 | 内容 | 安装位置 |
|---|---|---|
| `packages.<system>.windinput` | 汇总包（服务 + addon + 设置程序 + 桌面集成 + 许可证） | — |
| `packages.<system>.windinput-service` | `wind_input` 服务二进制 + `data/` 词库 | `$out/lib/windinput/{wind_input,data}`，另在 `$out/bin/wind_input` 放程序入口软链 |
| `packages.<system>.windinput-fcitx5` | `libwindinput.so` + addon/输入法描述 + 图标 + GNOME Shell 扩展 | `$out/lib/fcitx5`、`$out/share/fcitx5`、`$out/share/icons` |
| `packages.<system>.windinput-data` | 词库数据目录（等价于 `build_dev/data`） | 单独一个 store 路径 |
| `packages.<system>.wind-setting` | 设置程序 | `$out/lib/windinput/wind_setting` |
| `devShells.<system>.default` | cargo / cmake / ninja / Fcitx5 头文件 / xcb / wayland / gio | — |

`system` 覆盖 `x86_64-linux` 与 `aarch64-linux`。

汇总包另装许可证副本 `share/licenses/windinput/{LICENSE,NOTICE.md}`（与
`scripts/pack-installer.sh` 给 Windows 安装包装的 `licenses/` 同一份口径：Nix 包不在那条
产物链上，得自己带上）。

## 关键约束：数据目录必须与可执行文件同级

`wind_input` 用 `current_exe().parent()/data` 定位词库（`wind-config::variant::install_root`），
所以服务包自己就装成 `lib/windinput/{wind_input,data}`，**不靠软链**。汇总包用 `symlinkJoin`
是安全的：`current_exe()` 经 `/proc/self/exe` 解析到 `windinput-service` 的真实 store 路径，
那里的 `data/` 一直在。

`windinput-service` 的 `$out/bin/wind_input` 是同一条规则的唯一例外：Nix 的 `nix run` /
`lib.getExe` 按 `meta.mainProgram` 去找 `$out/bin/<名>`。它是**相对软链**，指回
`lib/windinput/wind_input` 这个真实文件，`/proc/self/exe` 解析后 `install_root` 仍是
`lib/windinput`，`data/` 定位不受影响。

addon 侧的服务 / 设置程序默认路径是源码里的 `#ifndef` 宏。`wind_linux/CMakeLists.txt` 因此提供
`WIND_SERVICE_PATH` / `WIND_SETTING_PATH` 两个 cache 选项（空 = 用源码内置的 `/usr/lib/...`），
由 `nix/addon.nix` 注入 store 路径；编译定义是 `PUBLIC`，单测读同一个值。
运行时仍可用 `WIND_INPUT_SERVICE` / `WIND_INPUT_SETTING` 覆盖。

## 词库数据流水线

等价于 `scripts/dev.sh` 的 `gen-data` + `assemble_data`（`nix/data.nix`）：

1. **上游源**：8 个 GitHub 仓库（rime-frost、pinyin-data、OpenCC、rime-wubi86-jidian、
   rime-lua-aux-code、rime-stroke、rime-emoji、hanzi-chars）按 **commit 钉版本**，
   由一个固定输出派生（`outputHashMode = "recursive"`）一次性 `curl` 下来。
   只取构建真正用到的文件 —— 不整仓拉取，CLDR 整仓也不需要（emoji 名表已入库）。
2. **生成工具**：`wind-tools` 的 `gen_unigram` / `gen_pinyin` / `gen_opencc` / `gen_dict` /
   `gen_aux_code`，作为独立派生构建后进 `PATH`。
3. **组装**：复制入库的 `data/`、（目录存在时）合并 `wind_input/data/settings/`、裁剪 rime-frost 的
   `import_tables`（`nix/strip-unshipped-imports.awk`）、过滤 rime-frost 分组
   （`nix/strip-frost-sections.awk`）、跑 `gen-compat.sh linux` 换掉 Windows 版兼容规则，
   最后断言关键文件非空（`pinyin_map.txt`、五笔主/扩展库、`stroke.dict.yaml`、
   `STCharacters.octrie`、emoji 表与 LICENSE）。

> `gen_dict` 的默认配置路径是编译期的 `CARGO_MANIFEST_DIR`，在 Nix 构建里不存在，
> 因此显式传 `--config wind_input/apps/wind-tools/data/gen_dict/gen_dict.toml`。

派生输入（`src`）按 `cleanSourceWith` 收窄到真正被读到的子树：`nix/data.nix` 只留 `data/`、
`scripts/lib/gen-compat.sh`、`wind_input/apps/wind-tools/data/`、`wind_input/data/`（第 3 步的
`settings/` 源，当前仓库没有该目录，白名单先留着）；`nix/addon.nix` 只留
`wind_linux/`、`wind_tsf/include/`、单测按 `WIND_PROTOCOL_RS` / `WIND_REPO_DIR` 读的
`wind_input/crates/{wind-ipc,wind-bridge,wind-ui,wind-keys}/src/` 与
`wind_macos/Sources/WindInputApp/UI/`；`nix/setting.nix` 见下节。不这么做的话，改一份
`README.md` 也会改掉 `windinput-data` 的输入哈希，级联重建 data → service → fcitx5 → 汇总包。
**代价**：从仓库新读一个文件就要把它的前缀补进对应白名单，漏了只在这条 Nix 链上失败。

`service.nix` 与 `data.nix` 的 tools 派生没法按子树收窄（要整棵 `wind_input/` 才能跑 cargo），
但都经 `nix/src-filter.nix` 剔掉 `target/`：`path:` 源下 flake 整棵树（含未跟踪文件）都会被
复制，本机每跑一次 `cargo build` 就会换一个 store 路径。设置程序的 `wind_input/` 子树同样
经这个过滤器。

## 设置程序

`wind-setting` 经 path 依赖引用 `../WindInput/wind_input/crates/{wind-ipc,wind-rpc,wind-config,
wind-punct,wind-transform}` 与 `../wind-ui-rust`。`nix/setting.nix` 先拼出三仓兄弟布局
（`WindInput/`、`wind-setting/`、`wind-ui-rust/`），再把 `wind-setting/Cargo.lock` 复制到布局根
（cargo 的 setup hook 只在源码根找锁文件），构建时 `cd wind-setting`。

布局里的 `WindInput/` 只放设置程序真正用到的两处子树：`wind_input/`（crates + 工作区根
`Cargo.toml`）与 `scripts/linux/pkg/`（`src/xdg_assoc.rs` 用 `include_str!` 内嵌其中的
`.desktop` / `mime.xml`）。不拷整仓，`docs/`、`data/`、`wind_linux/` 等无关文件的改动就不必
让设置程序重编。

两个邻仓作为 flake input 取（`flake.lock` 钉住 commit）。

## 版本号

`docs/VERSION` 是唯一真源；flake 读它后分别注入：

| 产物 | 注入方式 |
|---|---|
| `wind_input` 服务 | `apps/service/build.rs` 读 `docs/VERSION`（`service.nix` 的 `postPatch` 在构建根补齐该文件） |
| Fcitx5 addon | `-DWIND_ADDON_VERSION` → `share/fcitx5/addon/windinput.conf` 的 `Version=` |
| 设置程序 | 环境变量 `WIND_APP_VERSION`（`setting.nix`） |
| 词库数据 | `gen_dict --version-date`（词库头部，见上文） |

服务的二进制里嵌着构建时刻与 git revision（`apps/service/build.rs::emit_build_stamp`）。为让同一
派生在任意机器上得到同一输出：版本戳取 flake 源的 `lastModified`（git 源是提交时间，`path:` 源是
源码树 mtime），由 `flake.nix` 传给 `nix/service.nix` 的 `sourceDateEpoch`，后者再设成
`SOURCE_DATE_EPOCH`。**stdenv 自带的只是兜底常量 315532800（1980-01-01），不是源码 mtime**——
不显式传就会恒为 1980；`build.rs` 遇到畸形值（空串 / 非整数）直接构建失败，不退回当前时刻。不设
该变量时（deb / 本地构建）仍取当前时刻。revision 由 `WIND_GIT_HASH_OVERRIDE` 传入（沙箱里没有
`.git`，否则二进制里恒为 `unknown`）。两个变量都声明了 `cargo:rerun-if-env-changed`，同一个
`target/` 下改值也会重新执行 build script。动了这两处先跑
`nix build --rebuild path:.#windinput-service` 确认仍然可复现。

Nix 包不参与 `release.yml` / `linux-build.yml` 的产物链（那两个只出 Windows 安装包与 deb），
也还没有 CI 构建：用户从 flake input 或源码树自建。发版时只需确认 `docs/VERSION` 已改。

两条打包链共用同一份版本真源：deb 侧 `scripts/linux/build-deb.sh` 同样传
`-DWIND_ADDON_VERSION="$APP_VERSION"`（并经 `WIND_APP_VERSION` 给设置程序），因此 fcitx5
输入法列表里的 addon 版本在 deb 与 Nix 包上一致，都不会落回 `project(... VERSION 0.1.0)`。

## 使用

```bash
# 构建
nix build .#windinput

# 开发环境（cargo / cmake / fcitx5 头文件齐备）
nix develop
```

NixOS：

```nix
{
  inputs.windinput.url = "github:huanfeng/WindInput";

  # 在 nixosConfiguration 里：
  i18n.inputMethod = {
    enable = true;
    type = "fcitx5";
    fcitx5.addons = [ inputs.windinput.packages.${pkgs.stdenv.hostPlatform.system}.windinput ];
  };
}
```

模块会把 addon 包 join 进 `fcitx5-with-addons`，`FCITX_ADDON_DIRS` 指向
`$out/lib/fcitx5`、`XDG_DATA_DIRS` 带上 `$out/share` —— 本包的目录布局与之对齐。

其它发行版（用 Nix 但不跑 NixOS）：安装后把包里的 `lib/fcitx5` 与 `share/fcitx5` 加进
Fcitx5 的 `FCITX_ADDON_DIRS` / `FCITX_DATA_DIRS`，或自行包一层 `fcitx5-with-addons`。

装好后与 deb 版的使用方式一致：在「Fcitx5 配置」里加入「清风输入法」，服务由 addon 首次连接
失败时自动拉起。

## 维护

- **升级产品版本**：只改 `docs/VERSION`；flake 直接读该文件。
- **升级上游词库源**：改 `nix/data.nix` 里的 `*Rev`，构建会报 hash mismatch，把
  `outputHash` 换成报错里的 `got:` 值即可。
- **升级邻仓**：`nix flake update wind-setting wind-ui-rust`。
- **fcitx5 ABI**：addon 用 nixpkgs 的 fcitx5 头文件构建，`CMakeLists.txt` 会自动探测 C++20
  （新头文件需要），无需人工干预。

## 用户数据与配置位置（XDG）

Linux 走 `dirs` crate 的 XDG 解析（`wind-config::Config`）：

| 内容 | 位置 |
|---|---|
| 配置、用户词库、短语、方案覆盖（`config.toml`、`data/<schema>/…`） | `$XDG_CONFIG_HOME/WindInput`（默认 `~/.config/WindInput`） |
| 缓存（`.wdat` 等可重建产物）、日志、`state.toml` | `$XDG_DATA_HOME/WindInput`（默认 `~/.local/share/WindInput`） |
| 运行时 socket | `$XDG_RUNTIME_DIR/WindInput`（`WIND_INPUT_RUNTIME_DIR` 可覆盖；无 `XDG_RUNTIME_DIR` 时退 `/tmp/wind_input-<uid>`） |

这与安装位置（store）无关：Nix 包只提供只读的 `data/` 出厂数据；用户目录里的同名文件仍按
`Config::resolve_data_file` / `resolve_schema_resource` 的覆盖规则整体替代安装目录那份。
**不要**把用户数据写进 store，也不要把 `data/` 指到 `~/.config`。

## 已知限制

- 与 deb 版一致：GNOME Wayland 下光标定位依赖 Shell 扩展 `windinput-caret@windinput`
  （声明支持 Shell 47–50），KDE Wayland 需要在系统设置里把虚拟键盘选为 Fcitx 5。
  细节见 [wind_linux/README.md](../../wind_linux/README.md)。
- 桌面入口（设置 / 导入）与 `.wpkg` / `.wtheme` 的 MIME 定义由汇总包安装，`Exec` 已替换为
  store 路径；未包含 NixOS module（按当前范围，用户自行把 addon 挂进 fcitx5 配置）。
- **字体依赖与 deb 对齐**：fontconfig（dlopen 选字，服务与设置程序都 wrap 进
  `LD_LIBRARY_PATH`）随包；中文字体（deb 的 `fonts-noto-cjk`）与彩色 emoji 字体
  （`fonts-noto-color-emoji`，deb 仅 Recommends）**不带**，由系统字体集提供——NixOS 常见
  做法是把它们加进 `fonts.packages`。
- **词库版本戳固定**：`gen_dict` 带 `--version-date $version`（等于 `docs/VERSION`），
  词库头部不随构建日期漂移，数据 store 路径在两次发版之间保持稳定、可被 Nix 缓存命中。

## 已验证

- `nix build .#windinput-data`：37 MB 数据目录，五笔/拼音/辅助码/OpenCC/emoji 齐全。
- `nix build .#windinput-service`：服务从 store 启动，加载全部词库、建立 `bridge.sock`，
  收到 SIGTERM 正常落盘退出；二进制里的时间戳是 flake 源的 `lastModified`
  （本次实测 `wind_input 0.127.0 (build 2026-10-29T08:57:08Z git:unknown)`，不再是 stdenv
  兜底的 1980），revision 由 `WIND_GIT_HASH_OVERRIDE` 注入（`path:.` 构建得 `unknown`，
  git flake 得短哈希）。
- 派生输入收窄：`nix eval --raw .#windinput-service.src` 得到 23 MB 的源码副本，`target/`
  不在其中（`windinput-tools` 用的正是同一路径）；`.#wind-setting.src` 的兄弟布局里同样没有
  `wind_input/target/`。
- `nix build .#wind-setting`：`$out/lib/windinput/wind_setting` 是 16 KB 的 wrapper，真实文件
  是同目录的 `.wind_setting-wrapped`；闭包含 fontconfig，与服务走同一条选字路径。
- `nix build --rebuild .#windinput-service`：重建同一派生，输出与首次构建逐字节一致。
- `nix build .#windinput-fcitx5`：14 个纯逻辑单测全部通过；`checkPhase` 另在产物里断言服务与
  设置程序的 store 前缀（单测在宏未定义时也会过，见 `nix/addon.nix`）；
  addon conf 的 `Version=` 等于 `docs/VERSION`（当前 `0.127.0`），`Category=InputMethod` 在位。
- `nix build .#windinput`：汇总包布局正确，桌面入口 `Exec` 已指向 store。
- `nix develop -c cargo --version` 等：devShell 可用。
- `nix flake show path:. --all-systems`：5 个包 + `default` + devShell 在 x86_64 / aarch64 下均可求值。
