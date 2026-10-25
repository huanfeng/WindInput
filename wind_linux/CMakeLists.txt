# wind_linux：清风输入法的 Fcitx5 addon（C++17；新版 Fcitx5 头文件下自动升 C++20）。
#
#   eval "$(scripts/linux/bootstrap-sdk.sh env)"      # 无 root 机器上先引入用户前缀 SDK
#   cmake -S wind_linux -B wind_linux/build -G Ninja
#   cmake --build wind_linux/build
#   ctest --test-dir wind_linux/build --output-on-failure
#
# 选项：
#   WIND_BUILD_ADDON   构建 Fcitx5 addon（需 Fcitx5Core 开发包）；关掉则只编纯逻辑库与单测
#   WIND_VARIANT_DEV   编出 dev 变体（运行时目录 WindInputDev、SHM 名 /WindInputDev.<uid>）
cmake_minimum_required(VERSION 3.16)
project(wind_linux VERSION 0.1.0 LANGUAGES C CXX)   # C：wayland-scanner 生成的协议接口表是 C 代码

# 默认 C++17（Ubuntu 22.04 的 Fcitx5 5.0.x 头文件即可）；命令行 -DCMAKE_CXX_STANDARD=20 可覆盖。
# 较新的 Fcitx5 头文件要 C++20，addon 目标会在下面探测后自动升级。
if(NOT DEFINED CMAKE_CXX_STANDARD)
    set(CMAKE_CXX_STANDARD 17)
endif()
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

option(WIND_BUILD_ADDON "Build the Fcitx5 addon" ON)
option(WIND_VARIANT_DEV "Build the dev variant" OFF)

# ── 纯逻辑库（不依赖 Fcitx5，单测直接链接）──
add_library(windcore STATIC
    src/core/Bridge.cpp
    src/core/Codec.cpp
    src/core/GnomeCaretGeom.cpp
    src/core/HostUi.cpp
    src/core/KeyMap.cpp
    src/core/KeySynth.cpp
    src/core/Menu.cpp
    src/core/OverlayInput.cpp
    src/core/ResponseRouter.cpp
    src/core/RetiredSurfaces.cpp
    src/core/ServiceLauncher.cpp
    src/core/SettingsLauncher.cpp
    src/core/ShmFrame.cpp
    src/core/Utf.cpp
)
target_include_directories(windcore PUBLIC include)
target_compile_options(windcore PRIVATE -Wall -Wextra)
target_compile_definitions(windcore PRIVATE WIND_VARIANT_DEV=$<BOOL:${WIND_VARIANT_DEV}>)
find_package(Threads REQUIRED)
target_link_libraries(windcore PUBLIC Threads::Threads)

# ── 单测 ──
enable_testing()
foreach(t codec_test keymap_test router_test bridge_test protocol_sync_test shm_test launcher_test
        settings_launcher_test menu_test host_ui_test overlay_input_test keysynth_test
        retired_surfaces_test gnome_caret_geom_test)
    add_executable(${t} tests/${t}.cpp)
    target_link_libraries(${t} PRIVATE windcore)
    target_compile_definitions(${t} PRIVATE
        WIND_PROTOCOL_RS="${CMAKE_CURRENT_SOURCE_DIR}/../wind_input/crates/wind-ipc/src/protocol.rs"
        WIND_REPO_DIR="${CMAKE_CURRENT_SOURCE_DIR}/..")
    add_test(NAME ${t} COMMAND ${t})
endforeach()

# ── Fcitx5 addon ──
if(WIND_BUILD_ADDON)
    find_package(Fcitx5Core 5.0 REQUIRED)
    find_package(Fcitx5Utils 5.0 REQUIRED)
    find_package(PkgConfig REQUIRED)
    # 候选窗的 X11 呈现：自建 xcb 连接 + XShape（无合成器时按 alpha 抠形）+ RandR（工具栏 /
    # 软键盘按输出取名字与工作区）。
    pkg_check_modules(XCB REQUIRED IMPORTED_TARGET xcb xcb-shape xcb-randr)
    # gio 只服务 GNOME 光标扩展客户端，且那段代码在 WIND_HAVE_WAYLAND 里。没有 Wayland 时不探测、
    # 不链接；有 Wayland 但没有 gio 时其余 addon 照常编，只是不编 src/fcitx/GnomeCaret.cpp。
    set(WIND_GNOME_CARET_EXT OFF)

    add_library(windinput MODULE
        src/fcitx/WindEngine.cpp
        src/fcitx/X11Panel.cpp
    )
    target_link_libraries(windinput PRIVATE windcore Fcitx5::Core Fcitx5::Utils PkgConfig::XCB)

    # 新版 Fcitx5（Arch 等）的头文件用了 std::ranges / std::span / std::source_location，按当前标准编不过
    # 就把 addon 升到 C++20（GH#192）。只探 addon 用到的头文件与日志宏，纯逻辑库与单测不受影响。
    include(CheckCXXSourceCompiles)
    set(CMAKE_REQUIRED_LIBRARIES Fcitx5::Utils)
    check_cxx_source_compiles("
        #include <fcitx-utils/key.h>
        #include <fcitx-utils/log.h>
        FCITX_DEFINE_LOG_CATEGORY(probe_log, \"probe\");
        int main() { FCITX_LOGC(probe_log, Debug) << fcitx::Key(\"a\").toString(); return 0; }
    " WIND_FCITX_HEADERS_OK_AT_DEFAULT_STD)
    unset(CMAKE_REQUIRED_LIBRARIES)
    if(NOT WIND_FCITX_HEADERS_OK_AT_DEFAULT_STD)
        message(STATUS "Fcitx5 头文件需要 C++20，addon 按 C++20 编译")
        target_compile_features(windinput PRIVATE cxx_std_20)
    endif()

    # Wayland 候选窗（input-method-v2 popup surface）：要 libwayland-client 与 wayland-scanner。缺任何一样就不编这一路
    # （只剩 X11 候选窗，Wayland 原生应用里不显示）；发布构建里缺了应当是配置错误，由 verify-deb.sh 兜（见该脚本）。
    pkg_check_modules(WAYLAND_CLIENT IMPORTED_TARGET wayland-client)
    find_program(WAYLAND_SCANNER wayland-scanner)
    # Fcitx5ModuleWayland 的 cmake 配置把头文件目录写死成 /usr/include，用户前缀 SDK 里对不上，
    # 故自己找（从 Fcitx5Core 的头文件目录相对定位，系统安装时同样成立）。
    get_target_property(FCITX_CORE_INC Fcitx5::Core INTERFACE_INCLUDE_DIRECTORIES)
    find_path(FCITX_WAYLAND_INC wayland_public.h
        HINTS ${FCITX_CORE_INC}/../Module/fcitx-module/wayland
        PATH_SUFFIXES Fcitx5/Module/fcitx-module/wayland)
    if(WAYLAND_CLIENT_FOUND AND WAYLAND_SCANNER AND FCITX_WAYLAND_INC)
        # input-method-v1 只用其中的 zwp_input_panel_v1（KWin 的 overlay 面板）。
        # 协议文件随仓（MIT；input-method-v2 在 wayland-protocols 1.3x 起不再随包提供，viewporter /
        # xdg-output 一并随仓，免得各发行版的 wayland-protocols 版本与是否安装成为构建变量）。
        # wlr-layer-shell 来自 wlr-protocols（屏幕锚定的浮层用）；它的 get_popup 引用 xdg_popup，
        # 所以 xdg-shell 也要生成一份接口表。
        set(WL_GEN ${CMAKE_CURRENT_BINARY_DIR}/wayland-gen)
        file(MAKE_DIRECTORY ${WL_GEN})
        set(WL_GEN_SRC)
        foreach(proto input-method-unstable-v1 input-method-unstable-v2 viewporter xdg-output-unstable-v1
                      wlr-layer-shell-unstable-v1 xdg-shell
                      relative-pointer-unstable-v1)
            set(xml ${CMAKE_CURRENT_SOURCE_DIR}/protocols/${proto}.xml)
            add_custom_command(
                OUTPUT ${WL_GEN}/${proto}-client-protocol.h ${WL_GEN}/${proto}-protocol.c
                COMMAND ${WAYLAND_SCANNER} client-header ${xml} ${WL_GEN}/${proto}-client-protocol.h
                COMMAND ${WAYLAND_SCANNER} private-code ${xml} ${WL_GEN}/${proto}-protocol.c
                DEPENDS ${xml})
            list(APPEND WL_GEN_SRC ${WL_GEN}/${proto}-protocol.c)
        endforeach()
        add_library(windwlproto OBJECT ${WL_GEN_SRC})
        set_target_properties(windwlproto PROPERTIES POSITION_INDEPENDENT_CODE ON
            C_VISIBILITY_PRESET hidden)   # 与 Fcitx5 自己那份同名接口表互不干扰
        target_include_directories(windwlproto PUBLIC ${WL_GEN})
        target_link_libraries(windwlproto PUBLIC PkgConfig::WAYLAND_CLIENT)
        target_sources(windinput PRIVATE src/fcitx/WaylandPanel.cpp $<TARGET_OBJECTS:windwlproto>)
        target_include_directories(windinput PRIVATE ${WL_GEN} ${FCITX_WAYLAND_INC})
        target_link_libraries(windinput PRIVATE PkgConfig::WAYLAND_CLIENT)
        target_compile_definitions(windinput PRIVATE WIND_HAVE_WAYLAND=1)
        pkg_check_modules(GIO IMPORTED_TARGET gio-2.0)
        if(GIO_FOUND)
            target_sources(windinput PRIVATE src/fcitx/GnomeCaret.cpp)
            target_link_libraries(windinput PRIVATE PkgConfig::GIO)
            target_compile_definitions(windinput PRIVATE WIND_HAVE_GNOME_CARET=1)
            set(WIND_GNOME_CARET_EXT ON)
            message(STATUS "GNOME 光标扩展客户端：启用（gio-2.0）")
        else()
            message(STATUS "缺 gio-2.0：不编 GNOME 光标扩展客户端")
        endif()
        message(STATUS "Wayland 候选窗：启用")
    endif()
    if(NOT TARGET windwlproto)
        message(WARNING "缺 libwayland-client / wayland-scanner：不编 Wayland 候选窗")
    endif()
    target_compile_options(windinput PRIVATE -Wall -Wextra)
    # Fcitx5 按 addon conf 的 Library= 找 `lib<name>.so`；MODULE 默认就带 lib 前缀。
    set_target_properties(windinput PROPERTIES PREFIX "lib")

    # addon / 输入法描述文件：构建目录里也放一份，e2e 直接把构建目录当数据目录用，
    # 不必 install。
    configure_file(data/windinput-addon.conf.in
        ${CMAKE_CURRENT_BINARY_DIR}/share/fcitx5/addon/windinput.conf @ONLY)
    configure_file(data/windinput.conf.in
        ${CMAKE_CURRENT_BINARY_DIR}/share/fcitx5/inputmethod/windinput.conf @ONLY)

    include(GNUInstallDirs)
    install(TARGETS windinput DESTINATION ${CMAKE_INSTALL_LIBDIR}/fcitx5)
    install(FILES ${CMAKE_CURRENT_BINARY_DIR}/share/fcitx5/addon/windinput.conf
        DESTINATION ${CMAKE_INSTALL_DATADIR}/fcitx5/addon)
    install(FILES ${CMAKE_CURRENT_BINARY_DIR}/share/fcitx5/inputmethod/windinput.conf
        DESTINATION ${CMAKE_INSTALL_DATADIR}/fcitx5/inputmethod)
    # 图标（输入法条目 `windinput`、托盘随模式切换的 `windinput-zh*` / `-en` / `-caps`，以及
    # mimetypes/ 下方案包、主题包的文件类型图标）：成品入库，分别由 scripts/linux/gen-icons.py、
    # wind-ui 的 gen_tray_icons 例子与 scripts/linux/gen-mime-icons.py 一次性生成，构建与打包都
    # 不需要额外工具。
    install(DIRECTORY data/icons/hicolor DESTINATION ${CMAKE_INSTALL_DATADIR}/icons)
    # GNOME Shell 扩展：只在这次构建真的编了客户端时安装，避免没有 Wayland/gio 的包带一份用不上的扩展。
    # 启用步骤见 gnome-shell-extension/README.md（装上文件不会自动启用）。
    if(WIND_GNOME_CARET_EXT)
        install(DIRECTORY gnome-shell-extension/windinput-caret@windinput
            DESTINATION ${CMAKE_INSTALL_DATADIR}/gnome-shell/extensions)
    endif()
endif()
