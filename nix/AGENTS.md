# nix/ —— Linux 端的 Nix 打包

`flake.nix` + 本目录把 Linux（Fcitx5）形态打成可复现的 Nix 包。用户视角的用法与产出见
[docs/design/nix-packaging.md](../docs/design/nix-packaging.md)；这里只记**改这些文件前必须知道的约定**。

## 文件职责

| 文件 | 职责 |
|---|---|
| `../flake.nix` | 输出 `packages.{windinput,windinput-service,windinput-fcitx5,windinput-data,wind-setting}` 与 `devShells.default`；版本直接读 `docs/VERSION` |
| `data.nix` | 词库数据（等价于 `dev.sh` 的 gen-data + assemble_data）：上游 FOD + 生成工具 + 组装断言 |
| `service.nix` | Rust 服务；装成 `$out/lib/windinput/{wind_input,data}`，另在 `$out/bin/wind_input` 放一条相对软链（`meta.mainProgram` 的程序入口） |
| `addon.nix` | Fcitx5 addon（C++/CMake）+ 14 个单测；版本经 `WIND_ADDON_VERSION` 注入 |
| `setting.nix` | 图形设置程序（三仓兄弟布局；主仓只拷 `wind_input/` 与 `scripts/linux/pkg/`） |
| `package.nix` | 汇总包 + 桌面入口 / MIME + 许可证（`share/licenses/windinput/`） |
| `devshell.nix` | cargo / cmake / fcitx5 头文件 / xcb / wayland / gio |
| `src-filter.nix` | 派生输入的构建产物过滤：把 `target/` 从 `src` 副本里剔掉（service / tools / 设置程序共用） |
| `strip-*.awk` | 从 `scripts/dev.sh` 平移的 rime-frost 裁剪逻辑（改一边要同步另一边） |

## 立约级不变量（违反即静默故障）

- **`data/` 必须与可执行文件同级，且必须是真实目录**：服务用 `current_exe().parent()/data`
  找词库。汇总包是 `symlinkJoin`，靠 `/proc/self/exe` 解析到 service 的真实 store 路径；
  **不要**把服务二进制改成软链指向别处，也不要把 `data/` 挪到别层。
  `$out/bin/wind_input` 是唯一允许的软链：它只是给 `nix run` / `lib.getExe` 用的入口，
  `/proc/self/exe` 解析后 `install_root` 仍是 `$out/lib/windinput`（`data/` 始终在真实二进制的同级）。
- **dlopen 的字体库要靠 wrapper 找**：服务与设置程序共用 `windui` 的文字栈，运行期
  `dlopen("libfontconfig.so.1")`，ELF 无 NEEDED。`runtimeDependencies` 在本仓实测无效
  （cc-wrapper 按 `buildInputs` 写的 RUNPATH 会被随后的 stdenv `patchelf --shrink-rpath`
  当未使用条目剪掉），故两处都 `wrapProgram` 注入 `LD_LIBRARY_PATH`。
  验收看三条：`nix-store -qR` 闭包含 fontconfig、裸跑服务日志无 `dlopen libfontconfig.so.1
  失败`、`$out/lib/windinput/wind_setting` 是 wrapper 脚本（真实文件是 `.wind_setting-wrapped`）。
- **服务/设置程序的默认路径走 CMake 选项注入**：`WIND_SERVICE_PATH` / `WIND_SETTING_PATH`
  是 `wind_linux/CMakeLists.txt` 的 cache 选项（`PUBLIC` 编译定义）。`settings_launcher_test`
  不跟宏逐字比（`argv[0]` 与宏同源，比宏即恒真），断的是「绝对路径 + 末段 `wind_setting`」；
  宏没定义（deb / 源码构建）时才钉住 `/usr/lib/windinput/wind_setting` 字面量。
  不要改回构建期 `substituteInPlace` 源码字面量——那样宏不会被定义，单测会按「未注入」分支
  去比 `/usr/lib/windinput/wind_setting`，而源码已被替换成 store 路径，构建直接失败。
- **版本号只有 `docs/VERSION` 一个真源**：`flake.nix` 读它，再分别注入
  `WIND_ADDON_VERSION`（addon conf 的 `Version=`）与 `WIND_APP_VERSION`（设置程序 build.rs）。
  别在 `wind_linux/CMakeLists.txt` 的 `project(... VERSION ...)` 或各仓 `Cargo.toml` 里
  手改版本——那只会让 addon 列表显示的版本与产品版本脱钩。
- **不套 `wrapQtAppsHook` / `autoPatchelfHook`**：设置程序用 `windui`（X11 自绘，非 Qt），
  addon 链接的 Fcitx5 / xcb / wayland / glib 由 cc-wrapper 写进 RUNPATH（`readelf -d` 可验），
  服务与设置程序都只有 dlopen 的 fontconfig 需要 wrapper（见上一条）。加 Qt 依赖前先确认它
  真的进了链接。
- **`gen_dict` 必须显式给 `--config`**：默认配置路径是编译期 `CARGO_MANIFEST_DIR`，Nix 构建里
  不存在（配置目录里的 `custom_*.txt` / `boost_words.txt` 都相对它解析）。`--version-date`
  固定为 `docs/VERSION`，否则词库头部取当日 UTC，数据 store 路径会日日漂移；头部 `version:`
  是**版本戳不是日期**（该参数名叫 date 而已），wind-dict 只解析 `sort:` / `columns:`。
- **派生输入要收窄**：`data.nix` / `addon.nix` / `setting.nix` 的 `src` 都用 `cleanSourceWith`
  只留真正被读到的子树，别用整仓——改一个 `README.md` 也会改掉 `data.nix` 的输入哈希，级联
  重建 data → service → fcitx5 → 汇总包四层。
  `service.nix` / `data.nix` 的 tools 派生需要整棵 `wind_input/`（cargo workspace），走
  `src-filter.nix::withoutBuildOutput` 只剔 `target/`：`path:` 源下本机 cargo 产物否则会进
  store，每跑一次 `cargo build` 就换一个哈希。
  新增「构建期 / 单测从仓库读的文件」时，把它的前缀加进对应 `filter` 白名单：漏了只会在这条
  Nix 链上失败（cargo 照常编得过），`addon.nix` 顶部逐条列了单测读的每一处。
- **源码树在 store 里只读**：所有中间产物写 `$NIX_BUILD_TOP`；从 store 复制出来的目录要
  `chmod -R u+w` 才能继续写入。
- **`cargoRoot` 在本版 nixpkgs 的 cargo hook 里不生效**：cargo 的 setup hook 只在源码根找
  `Cargo.lock`。服务因此以 `wind_input/` 为 `src`；设置程序把 `wind-setting/Cargo.lock`
  复制到三仓布局根，再 `preBuild = "cd wind-setting"`。
- **三处编译期读仓库根**：`apps/service/build.rs` 与 `crates/wind-coordinator/build.rs` 各读一次
  `docs/VERSION`（后者供「关于」菜单）、`wind-theme` 用 `include_str!` 读
  `data/themes/_base/theme.toml`。`service.nix` 的 `postPatch` 在 `$NIX_BUILD_TOP` 补齐这两份
  文件（`docs/VERSION` 两处 build.rs 共用）；加新的跨目录 `include_str!` 时要同步补。
- **设置程序也编译期读主仓**：`wind-setting/src/xdg_assoc.rs` 用 `include_str!` 内嵌
  `scripts/linux/pkg/windinput-{setting,import}.desktop` 与 `windinput-mime.xml`。
  `setting.nix` 因此把该目录按原相对位置（`WindInput/scripts/linux/pkg/`）拷进兄弟布局——
  只拷 `wind_input/` 会以 `couldn't read .../windinput-setting.desktop` 报错。
- **许可证随汇总包分发**：`LICENSE` + `NOTICE.md` 装进 `$out/share/licenses/windinput/`
  （`package.nix` 的 `postBuild`）。与 `scripts/pack-installer.sh` 给安装包装的 `licenses/`
  是同一份口径：Nix 包不参与那条产物链，得自己带上，否则拿到手的人看不到上游声明。
  rime-emoji 的 LGPL 正文不走这里——它随 `data/emoji/` 由 `data.nix` 分发。

## 维护

- 升级产品版本：只改 `docs/VERSION`。
- 升级上游词库源：改 `data.nix` 的 `*Rev`，构建报 hash mismatch 后把 `got:` 填回 `outputHash`。
- 升级邻仓：`nix flake update wind-setting wind-ui-rust`。
- 验收基线（改动后至少跑一遍）：`nix build .#windinput`；addon 构建日志应有
  `Wayland 候选窗：启用` 与 14/14 测试通过；裸跑服务应加载 4 个引擎且无 dlopen 警告；
  `wind_setting` 与 `wind_input` 都应是 16 KB 的 wrapper（真实文件是同目录的
  `.<名>-wrapped`），漏了 wrapper 在 NixOS 上会静默退回字体目录扫描。
- 只查输出不构建：`nix flake show` / `nix eval .#packages.x86_64-linux --apply builtins.attrNames`。
- `nix flake check` 默认只查当前 system；要连 aarch64 一起求值用 `--all-systems`
  （会额外构建/求值 devShell，慢）。

## 已知不在本包范围

- Windows / macOS 形态（需原生 MSVC / Xcode）。
- NixOS module（用户自行把汇总包加进 `i18n.inputMethod.fcitx5.addons`）。
- 中文字体与彩色 emoji 字体（由系统字体集提供，同 deb 的 Depends / Recommends 分层）。
