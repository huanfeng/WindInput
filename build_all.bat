@echo off
setlocal

echo ======================================
echo WindInput - Build All
echo ======================================
echo.

REM 获取脚本目录
set "SCRIPT_DIR=%~dp0"

REM Wails 构建模式: debug(默认),release,skip
set "WAILS_MODE=debug"
if /I "%~1"=="-wails-debug" set "WAILS_MODE=debug"
if /I "%~1"=="-wails-release" set "WAILS_MODE=release"
if /I "%~1"=="-wails-skip" set "WAILS_MODE=skip"
if /I "%~1"=="debug" set "WAILS_MODE=debug"
if /I "%~1"=="release" set "WAILS_MODE=release"
if /I "%~1"=="skip" set "WAILS_MODE=skip"

echo [1/5] 构建 Go 服务(wind_input.exe)...
if not exist "%SCRIPT_DIR%build" mkdir "%SCRIPT_DIR%build"
cd "%SCRIPT_DIR%wind_input"
go build -o ../build/wind_input.exe ./cmd/service
if %errorLevel% neq 0 (
    echo [错误] Go 构建失败
    pause
    exit /b 1
)
echo Go 服务构建成功
echo.

echo [2/5] 构建 C++ DLL(wind_tsf.dll)...
if not exist "%SCRIPT_DIR%wind_tsf\build" mkdir "%SCRIPT_DIR%wind_tsf\build"
cd "%SCRIPT_DIR%wind_tsf\build"
if not exist "%SCRIPT_DIR%wind_tsf\build\CMakeCache.txt" (
    cmake ..
)
cmake --build . --config Release
if %errorLevel% neq 0 (
    echo [错误] C++ 构建失败
    pause
    exit /b 1
)
echo C++ DLL 构建成功
echo.

echo [3/5] 构建设置界面(wind_setting.exe)...
if /I "%WAILS_MODE%"=="skip" (
    echo [提示] 已按参数跳过 Wails 构建
) else (
    cd "%SCRIPT_DIR%wind_setting"
    REM 检查是否安装 wails
    where wails >nul 2>&1
    if %errorLevel% neq 0 (
        echo [警告] 未找到 Wails CLI,已跳过 wind_setting 构建
        echo        安装命令: go install github.com/wailsapp/wails/v2/cmd/wails@latest
    ) else (
        set "WAILS_FLAGS="
        if /I "%WAILS_MODE%"=="debug" set "WAILS_FLAGS=-debug"
        wails build %WAILS_FLAGS%
        if %errorLevel% neq 0 (
            echo [警告] wind_setting 构建失败,继续后续步骤...
        ) else (
            copy /Y "%SCRIPT_DIR%wind_setting\build\bin\wind_setting.exe" "%SCRIPT_DIR%build\" >nul
            if /I "%WAILS_MODE%"=="debug" (
                echo 设置界面构建成功 ^(debug 模式,可按 F12 打开 DevTools^)
            ) else (
                echo 设置界面构建成功 ^(release 模式^)
            )
        )
    )
)
echo.

echo [4/5] 复制词库到 build 目录...
cd "%SCRIPT_DIR%"
if not exist "%SCRIPT_DIR%build\dict\pinyin" mkdir "%SCRIPT_DIR%build\dict\pinyin"
if not exist "%SCRIPT_DIR%build\dict\wubi" mkdir "%SCRIPT_DIR%build\dict\wubi"

REM 复制拼音词库
if exist "%SCRIPT_DIR%ref\简全拼音库5.0.txt" (
    copy /Y "%SCRIPT_DIR%ref\简全拼音库5.0.txt" "%SCRIPT_DIR%build\dict\pinyin\pinyin.txt" >nul
    echo   - 已复制拼音词库
) else (
    echo [警告] ref 目录中未找到拼音词库
)

REM 复制五笔词库
if exist "%SCRIPT_DIR%ref\极爽词库6.txt" (
    copy /Y "%SCRIPT_DIR%ref\极爽词库6.txt" "%SCRIPT_DIR%build\dict\wubi\wubi86.txt" >nul
    echo   - 已复制五笔词库
) else (
    echo [警告] ref 目录中未找到五笔词库
)

REM 复制常用字表
if exist "%SCRIPT_DIR%dict\common_chars.txt" (
    copy /Y "%SCRIPT_DIR%dict\common_chars.txt" "%SCRIPT_DIR%build\dict\common_chars.txt" >nul
    echo   - 已复制常用字表
) else (
    echo [警告] 未找到常用字表
)
echo.

echo [5/5] 检查输出文件...
if not exist "%SCRIPT_DIR%build\wind_tsf.dll" (
    echo [错误] 未找到 wind_tsf.dll
    pause
    exit /b 1
)

if not exist "%SCRIPT_DIR%build\wind_input.exe" (
    echo [错误] 未找到 wind_input.exe
    pause
    exit /b 1
)

echo.
echo ======================================
echo 构建完成！
echo ======================================
echo.
echo 输出文件:
echo - build\wind_tsf.dll(TSF 桥接)
echo - build\wind_input.exe(输入法服务)
echo - build\wind_setting.exe(设置界面)
echo - build\dict\pinyin\pinyin.txt(拼音词库)
echo - build\dict\wubi\wubi86.txt(五笔词库)
echo - build\dict\common_chars.txt(常用字表)
echo.
echo 开发调试:
echo   cd build ^&^& wind_input.exe -log debug
echo.
echo 安装:
echo   以管理员身份运行 installer\install.bat
echo.
echo Wails 构建选项:
echo   build_all.bat -wails-debug   (默认)
echo   build_all.bat -wails-release
echo   build_all.bat -wails-skip
echo.

