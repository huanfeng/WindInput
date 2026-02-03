cmake_minimum_required(VERSION 3.15)
project(WindTsf)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# 设置输出目录（对多配置生成器也生效）
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY ${CMAKE_SOURCE_DIR}/../build)
set(CMAKE_LIBRARY_OUTPUT_DIRECTORY ${CMAKE_SOURCE_DIR}/../build)

# 对于多配置生成器（如 Visual Studio），显式设置每个配置的输出目录
foreach(CONFIG ${CMAKE_CONFIGURATION_TYPES})
    string(TOUPPER ${CONFIG} CONFIG_UPPER)
    set(CMAKE_RUNTIME_OUTPUT_DIRECTORY_${CONFIG_UPPER} ${CMAKE_SOURCE_DIR}/../build)
    set(CMAKE_LIBRARY_OUTPUT_DIRECTORY_${CONFIG_UPPER} ${CMAKE_SOURCE_DIR}/../build)
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
    src/CaretEditSession.cpp
    src/DisplayAttributeInfo.cpp
    src/HotkeyManager.cpp
)

# Resource file for icon
set(RESOURCE_FILE ${CMAKE_SOURCE_DIR}/res/resource.rc)

# 创建 DLL
add_library(wind_tsf SHARED ${SOURCES} ${RESOURCE_FILE})

# 注意：库通过源文件中的 #pragma comment 链接

# 添加预处理器定义
target_compile_definitions(wind_tsf PRIVATE
    UNICODE
    _UNICODE
    _WINDOWS
    _USRDLL
)

# 添加编译选项
target_compile_options(wind_tsf PRIVATE
    /utf-8  # 使用 UTF-8 编码
    /W3     # 警告级别3
)

# 生成模块定义文件
set(DEF_FILE ${CMAKE_SOURCE_DIR}/wind_tsf.def)
if(EXISTS ${DEF_FILE})
    target_sources(wind_tsf PRIVATE ${DEF_FILE})
endif()
