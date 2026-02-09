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

echo [1/7] 构建 Go 服务(wind_input.exe)...
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

echo [2/7] 构建 C++ DLL(wind_tsf.dll)...
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

echo [3/7] 构建设置界面(wind_setting.exe)...
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
        if /I "%WAILS_MODE%"=="debug" (
            wails build -debug
        ) else (
            wails build
        )
        if errorlevel 1 (
            echo [错误] wind_setting 构建失败,终止后续流程。
            exit /b 1
        )
        if not exist "%SCRIPT_DIR%wind_setting\build\bin\wind_setting.exe" (
            echo [错误] wind_setting.exe 未生成,终止后续流程。
            exit /b 1
        )
        copy /Y "%SCRIPT_DIR%wind_setting\build\bin\wind_setting.exe" "%SCRIPT_DIR%build\" >nul
        if /I "%WAILS_MODE%"=="debug" (
            echo 设置界面构建成功 ^(debug 模式,可按 F12 打开 DevTools^)
        ) else (
            echo 设置界面构建成功 ^(release 模式^)
        )
    )
)
echo.

echo [4/7] 下载拼音词库(rime-ice)...
cd "%SCRIPT_DIR%"
if not exist "%SCRIPT_DIR%build\dict\rime" mkdir "%SCRIPT_DIR%build\dict\rime"

set "RIME_BASE_URL=https://raw.githubusercontent.com/iDvel/rime-ice/main/cn_dicts"
set "RIME_DIR=%SCRIPT_DIR%build\dict\rime"

call :download_rime_file 8105.dict.yaml "单字词库, 约112KB"
if errorlevel 1 exit /b 1
call :download_rime_file base.dict.yaml "基础词库, 约16MB"
if errorlevel 1 exit /b 1
call :download_rime_file tencent.dict.yaml "腾讯词频, 约17MB"
if errorlevel 1 exit /b 1
echo.

echo [5/7] 准备词库文件...
cd "%SCRIPT_DIR%"
if not exist "%SCRIPT_DIR%build\dict\pinyin" mkdir "%SCRIPT_DIR%build\dict\pinyin"
if not exist "%SCRIPT_DIR%build\dict\wubi" mkdir "%SCRIPT_DIR%build\dict\wubi"

REM 复制拼音词库（Rime 格式）
if exist "%RIME_DIR%\8105.dict.yaml" (
    copy /Y "%RIME_DIR%\8105.dict.yaml" "%SCRIPT_DIR%build\dict\pinyin\8105.dict.yaml" >nul
    echo   - 已复制拼音单字词库 8105.dict.yaml
) else (
    echo [警告] 未找到 8105.dict.yaml
)
if exist "%RIME_DIR%\base.dict.yaml" (
    copy /Y "%RIME_DIR%\base.dict.yaml" "%SCRIPT_DIR%build\dict\pinyin\base.dict.yaml" >nul
    echo   - 已复制拼音基础词库 base.dict.yaml
) else (
    echo [警告] 未找到 base.dict.yaml
)

REM 生成 Unigram 语言模型（如果不存在）
if not exist "%SCRIPT_DIR%dict\pinyin" mkdir "%SCRIPT_DIR%dict\pinyin"
if not exist "%SCRIPT_DIR%dict\pinyin\unigram.txt" (
    echo   - 生成 Unigram 语言模型...
    cd "%SCRIPT_DIR%wind_input"
    go run ./cmd/gen_unigram -rime "%RIME_DIR%" -output "%SCRIPT_DIR%dict\pinyin\unigram.txt"
    if errorlevel 1 (
        echo [警告] Unigram 生成失败,智能组句功能不可用
    ) else (
        echo   - Unigram 语言模型生成成功
    )
    cd "%SCRIPT_DIR%"
) else (
    echo   - Unigram 语言模型已存在
)

REM 复制 Unigram 语言模型
if exist "%SCRIPT_DIR%dict\pinyin\unigram.txt" (
    copy /Y "%SCRIPT_DIR%dict\pinyin\unigram.txt" "%SCRIPT_DIR%build\dict\pinyin\unigram.txt" >nul
    echo   - 已复制 Unigram 语言模型
) else (
    echo [提示] Unigram 语言模型不存在,智能组句功能不可用
)

echo.

echo [6/7] 生成预编译二进制词库(.wdb)...
cd "%SCRIPT_DIR%wind_input"
set "PINYIN_DICT_DIR=%SCRIPT_DIR%build\dict\pinyin"
set "UNIGRAM_SRC=%SCRIPT_DIR%dict\pinyin\unigram.txt"

REM 检查源文件是否存在
if not exist "%PINYIN_DICT_DIR%\8105.dict.yaml" (
    echo [警告] 拼音词库不存在,跳过 .wdb 生成
    goto :skip_wdb
)
if not exist "%UNIGRAM_SRC%" (
    echo [警告] unigram.txt 不存在,跳过 unigram.wdb 生成
)

echo   - 生成 pinyin.wdb + unigram.wdb ...
go run ./cmd/gen_bindict -dict "%PINYIN_DICT_DIR%" -unigram "%UNIGRAM_SRC%" -out "%PINYIN_DICT_DIR%"
if errorlevel 1 (
    echo [警告] .wdb 生成失败,运行时将自动从源文件转换（首次启动稍慢）
) else (
    echo   - pinyin.wdb 生成成功
    echo   - unigram.wdb 生成成功
)

:skip_wdb
cd "%SCRIPT_DIR%"
echo.

REM 复制五笔词库
if exist "%SCRIPT_DIR%ref\极爽词库6.txt" (
    copy /Y "%SCRIPT_DIR%ref\极爽词库6.txt" "%SCRIPT_DIR%build\dict\wubi\wubi86.txt" >nul
    echo   - 已复制五笔词库
) else (
    echo [警告] ref 目录中未找到五笔词库
)

REM 预编译五笔二进制词库（加速首次启动）
if exist "%SCRIPT_DIR%build\dict\wubi\wubi86.txt" (
    echo   - 生成 wubi.wdb ...
    cd "%SCRIPT_DIR%wind_input"
    go run ./cmd/gen_wubi_wdb -src "%SCRIPT_DIR%build\dict\wubi\wubi86.txt" -out "%SCRIPT_DIR%build\dict\wubi"
    if errorlevel 1 (
        echo [提示] wubi.wdb 生成失败,运行时将自动转换
    ) else (
        echo   - wubi.wdb 生成成功
    )
    cd "%SCRIPT_DIR%"
)

REM 复制常用字表
if exist "%SCRIPT_DIR%dict\common_chars.txt" (
    copy /Y "%SCRIPT_DIR%dict\common_chars.txt" "%SCRIPT_DIR%build\dict\common_chars.txt" >nul
    echo   - 已复制常用字表
) else (
    echo [警告] 未找到常用字表
)
echo.

echo [7/7] 检查输出文件...
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
echo - build\dict\pinyin\pinyin.wdb(预编译拼音词库)
echo - build\dict\pinyin\unigram.wdb(预编译语言模型)
echo - build\dict\pinyin\8105.dict.yaml(拼音单字词库, wdb 转换源)
echo - build\dict\pinyin\base.dict.yaml(拼音基础词库, wdb 转换源)
echo - build\dict\pinyin\unigram.txt(Unigram 语言模型, wdb 转换源)
echo - build\dict\wubi\wubi86.txt(五笔词库, wdb 转换源)
echo - build\dict\wubi\wubi.wdb(预编译五笔词库)
echo - build\dict\common_chars.txt(常用字表)
echo.
echo 词库来源: 雾凇拼音 rime-ice (https://github.com/iDvel/rime-ice)
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
goto :eof

REM ======== 子例程 ========

:download_rime_file
REM 参数: %1=文件名  %2=描述
if exist "%RIME_DIR%\%~1" (
    echo   - %~1 已存在,跳过下载
    exit /b 0
)
echo   - 下载 %~1 (%~2)...
powershell -Command "Invoke-WebRequest -Uri '%RIME_BASE_URL%/%~1' -OutFile '%RIME_DIR%\%~1' -UseBasicParsing"
if errorlevel 1 (
    echo [错误] 下载 %~1 失败
    pause
    exit /b 1
)
exit /b 0
