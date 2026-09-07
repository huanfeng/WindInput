cmake_minimum_required(VERSION 3.15)
project(WindTsf)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# 静态链接 MSVC CRT(/MT):DLL 自包含,目标机无需安装 VC++ 运行库。
# TSF DLL 会被注入任意第三方宿主进程,动态 CRT(/MD)会新增 VCRUNTIME140/MSVCP140
# 依赖 —— 在未装运行库的宿主里等同"加载失败"。发布产物必须保持零外部运行库依赖
# (与旧交叉编译产物一致)。需 CMake ≥3.15 的 CMP0091(本文件 minimum 3.15 → 默认 NEW)。
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>")

# Debug variant option: builds with separate CLSID/pipe names for coexistence
option(WIND_DEV_VARIANT "Build debug variant with separate CLSID/pipe names" OFF)

if(WIND_DEV_VARIANT)
    add_definitions(-DWIND_DEV_VARIANT)
    set(TSF_TARGET_NAME "wind_tsf_dev")
else()
    set(TSF_TARGET_NAME "wind_tsf")
endif()

# 版本信息（由构建脚本通过 -D 传入，默认为开发版本）
if(NOT DEFINED APP_VERSION_MAJOR)
    set(APP_VERSION_MAJOR 0)
endif()
if(NOT DEFINED APP_VERSION_MINOR)
    set(APP_VERSION_MINOR 0)
endif()
if(NOT DEFINED APP_VERSION_PATCH)
    set(APP_VERSION_PATCH 0)
endif()
if(NOT DEFINED APP_VERSION_BUILD)
    set(APP_VERSION_BUILD 0)
endif()
if(NOT DEFINED APP_VERSION_STR)
    set(APP_VERSION_STR "${APP_VERSION_MAJOR}.${APP_VERSION_MINOR}.${APP_VERSION_PATCH}")
endif()
# 从模板生成版本资源文件
configure_file(
    ${CMAKE_SOURCE_DIR}/res/version.rc.in
    ${CMAKE_BINARY_DIR}/version.rc
    @ONLY
)

# 设置输出目录（对多配置生成器也生效）
if(WIND_DEV_VARIANT)
    set(TSF_OUTPUT_DIR ${CMAKE_SOURCE_DIR}/../build_dev)
else()
    set(TSF_OUTPUT_DIR ${CMAKE_SOURCE_DIR}/../build)
endif()
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY ${TSF_OUTPUT_DIR})
set(CMAKE_LIBRARY_OUTPUT_DIRECTORY ${TSF_OUTPUT_DIR})

# 对于多配置生成器（如 Visual Studio），显式设置每个配置的输出目录
foreach(CONFIG ${CMAKE_CONFIGURATION_TYPES})
    string(TOUPPER ${CONFIG} CONFIG_UPPER)
    set(CMAKE_RUNTIME_OUTPUT_DIRECTORY_${CONFIG_UPPER} ${TSF_OUTPUT_DIR})
    set(CMAKE_LIBRARY_OUTPUT_DIRECTORY_${CONFIG_UPPER} ${TSF_OUTPUT_DIR})
endforeach()

# 添加头文件目录
include_directories(${CMAKE_SOURCE_DIR}/include)
include_directories(${CMAKE_SOURCE_DIR}/src)

# Source files
set(SOURCES
    src/dllmain.cpp
    src/Globals.cpp
    src/ClassFactory.cpp
    src/Register.cpp
    src/TextService.cpp
    src/KeyEventSink.cpp
    src/IPCClient.cpp
    src/LangBarItemButton.cpp
    src/IconShmReader.cpp
    src/CaretEditSession.cpp
    src/DisplayAttributeInfo.cpp
    src/HotkeyManager.cpp
    src/HostWindow.cpp
    src/FileLogger.cpp
)

# Resource files (icon + version info)
set(RESOURCE_FILES
    ${CMAKE_SOURCE_DIR}/res/resource.rc
    ${CMAKE_BINARY_DIR}/version.rc
)

# 创建 DLL
add_library(${TSF_TARGET_NAME} SHARED ${SOURCES} ${RESOURCE_FILES})

# 注意：库通过源文件中的 #pragma comment 链接

# 添加预处理器定义
target_compile_definitions(${TSF_TARGET_NAME} PRIVATE
    UNICODE
    _UNICODE
    _WINDOWS
    _USRDLL
)

# 添加编译选项
#
# 警告级别与 Makefile(clang 交叉编译)那条路径保持同一口径, 见 wind_tsf/Makefile 的 WARNFLAGS。
# 两条路径此前不一致: 这里 /W3、那里一个 -W 都没有, 于是 CI 对 C++ 警告实际上是全盲的。
target_compile_options(${TSF_TARGET_NAME} PRIVATE
    /utf-8   # 使用 UTF-8 编码
    /W4      # 警告级别4
    /wd4100  # 未引用的形参: COM 接口与系统回调(DllMain/ITfKeyEventSink/ITfLangBarItem 等)
             # 签名由接口定义, 用不到的参数是常态而非漏用。全仓 23 处已逐一核对确认。
)

# 从模板生成模块定义文件（LIBRARY 名称随 TSF_TARGET_NAME 变化）
configure_file(
    ${CMAKE_SOURCE_DIR}/wind_tsf.def.in
    ${CMAKE_BINARY_DIR}/${TSF_TARGET_NAME}.def
    @ONLY
)
target_sources(${TSF_TARGET_NAME} PRIVATE ${CMAKE_BINARY_DIR}/${TSF_TARGET_NAME}.def)

# ── 单元测试（默认 OFF，不影响 DLL 构建）───────────────────────────────────
#
# 目前只有 SkipKeyTable。本目录绝大多数代码与 TSF/COM 纠缠、测不动；SkipKeyTable 是
# 被刻意剥出来的纯逻辑（不含任何 Win32 头，时钟由调用方以 GetTickCount64 传入），
# 所以能测。PairEngine、ClassifyInputKey 这类纯判据日后可照同一条路补进来。
#
# 本机跑（不需要 MSVC，msys2 的 g++ 即可，这是主要跑法）：
#   g++ -std=c++17 -Iwind_tsf/include -o skip_test wind_tsf/tests/skip_key_table_test.cpp && ./skip_test
# 经 CMake 跑（需 MSVC，本机无，未实跑；但 configure 阶段每次构建都会走到，语法错
# 误跑 dev.ps1 dm1 就会暴露）：
#   cmake -S wind_tsf -B build/tsf-tests -DWIND_TSF_TESTS=ON && cmake --build build/tsf-tests && ctest --test-dir build/tsf-tests
option(WIND_TSF_TESTS "Build wind_tsf unit tests" OFF)
if(WIND_TSF_TESTS)
    enable_testing()
    add_executable(skip_key_table_test tests/skip_key_table_test.cpp)
    target_include_directories(skip_key_table_test PRIVATE ${CMAKE_SOURCE_DIR}/include)
    if(MSVC)
        target_compile_options(skip_key_table_test PRIVATE /utf-8 /W4)
    endif()
    add_test(NAME skip_key_table COMMAND skip_key_table_test)
endif()
