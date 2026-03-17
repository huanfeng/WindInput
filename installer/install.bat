@echo off
setlocal
chcp 65001 >nul

echo ======================================
echo 清风输入法安装程序
echo ======================================
echo.

REM 检查管理员权限
net session >nul 2>&1
if %errorLevel% neq 0 (
    echo [错误] 请以管理员身份运行此脚本！
    echo 右键该文件并选择“以管理员身份运行”
    pause
    exit /b 1
)

REM 获取脚本目录
set SCRIPT_DIR=%~dp0
set BUILD_DIR=%SCRIPT_DIR%..\build

echo [1/10] 检查文件...
if not exist "%BUILD_DIR%\wind_tsf.dll" (
    echo [错误] 未找到 wind_tsf.dll
    echo 请先运行 build_all.bat
    pause
    exit /b 1
)

if not exist "%BUILD_DIR%\wind_dwrite.dll" (
    echo [错误] 未找到 wind_dwrite.dll
    echo 请先运行 build_all.bat
    pause
    exit /b 1
)

if not exist "%BUILD_DIR%\wind_input.exe" (
    echo [错误] 未找到 wind_input.exe
    echo 请先运行 build_all.bat
    pause
    exit /b 1
)

echo [2/10] 停止旧进程...
taskkill /F /IM wind_input.exe >nul 2>&1
timeout /t 1 /nobreak >nul

echo [3/10] 创建安装目录...
echo [4/10] 处理已有文件...
set "INSTALL_DIR=%ProgramW6432%\WindInput"
if "%ProgramW6432%"=="" set "INSTALL_DIR=%ProgramFiles%\WindInput"
if not exist "%INSTALL_DIR%" mkdir "%INSTALL_DIR%"

REM 生成随机后缀
set "RANDOM_SUFFIX=%RANDOM%%RANDOM%"

REM 处理旧 DLL - 先删除,失败则改名
if exist "%INSTALL_DIR%\wind_tsf.dll" (
    REM 先注销旧 DLL
    regsvr32 /u /s "%INSTALL_DIR%\wind_tsf.dll" >nul 2>&1

    del /F "%INSTALL_DIR%\wind_tsf.dll" >nul 2>&1
    if exist "%INSTALL_DIR%\wind_tsf.dll" (
        echo [WARN] Failed to delete old wind_tsf.dll, renaming to wind_tsf.dll.old_%RANDOM_SUFFIX%
        ren "%INSTALL_DIR%\wind_tsf.dll" "wind_tsf.dll.old_%RANDOM_SUFFIX%" >nul 2>&1
        if exist "%INSTALL_DIR%\wind_tsf.dll" (
            echo [WARN] Failed to rename old wind_tsf.dll, trying backup name...
            ren "%INSTALL_DIR%\wind_tsf.dll" "wind_tsf_%RANDOM_SUFFIX%.dll.bak" >nul 2>&1
        )
    )
)

REM 处理旧 wind_dwrite.dll
if exist "%INSTALL_DIR%\wind_dwrite.dll" (
    del /F "%INSTALL_DIR%\wind_dwrite.dll" >nul 2>&1
    if exist "%INSTALL_DIR%\wind_dwrite.dll" (
        echo [WARN] Failed to delete old wind_dwrite.dll, renaming to wind_dwrite.dll.old_%RANDOM_SUFFIX%
        ren "%INSTALL_DIR%\wind_dwrite.dll" "wind_dwrite.dll.old_%RANDOM_SUFFIX%" >nul 2>&1
        if exist "%INSTALL_DIR%\wind_dwrite.dll" (
            echo [WARN] Failed to rename old wind_dwrite.dll, trying backup name...
            ren "%INSTALL_DIR%\wind_dwrite.dll" "wind_dwrite_%RANDOM_SUFFIX%.dll.bak" >nul 2>&1
        )
    )
)

REM 处理旧 wind_input.exe
if exist "%INSTALL_DIR%\wind_input.exe" (
    del /F "%INSTALL_DIR%\wind_input.exe" >nul 2>&1
    if exist "%INSTALL_DIR%\wind_input.exe" (
        echo [WARN] Failed to delete old wind_input.exe, renaming to wind_input.exe.old_%RANDOM_SUFFIX%
        ren "%INSTALL_DIR%\wind_input.exe" "wind_input.exe.old_%RANDOM_SUFFIX%" >nul 2>&1
    )
)

REM 处理旧 wind_setting.exe
if exist "%INSTALL_DIR%\wind_setting.exe" (
    del /F "%INSTALL_DIR%\wind_setting.exe" >nul 2>&1
    if exist "%INSTALL_DIR%\wind_setting.exe" (
        echo [WARN] Failed to delete old wind_setting.exe, renaming to backup file
        ren "%INSTALL_DIR%\wind_setting.exe" "wind_setting.exe.old_%RANDOM_SUFFIX%" >nul 2>&1
    )
)

echo [5/10] 复制文件...
copy /Y "%BUILD_DIR%\wind_tsf.dll" "%INSTALL_DIR%\" >nul
if %errorLevel% neq 0 (
    echo [错误] 复制 wind_tsf.dll 失败
    pause
    exit /b 1
)

copy /Y "%BUILD_DIR%\wind_dwrite.dll" "%INSTALL_DIR%\" >nul
if %errorLevel% neq 0 (
    echo [错误] 复制 wind_dwrite.dll 失败
    pause
    exit /b 1
)

copy /Y "%BUILD_DIR%\wind_input.exe" "%INSTALL_DIR%\" >nul
if %errorLevel% neq 0 (
    echo [错误] 复制 wind_input.exe 失败
    pause
    exit /b 1
)

REM 复制 wind_setting.exe(可选)
if exist "%BUILD_DIR%\wind_setting.exe" (
    copy /Y "%BUILD_DIR%\wind_setting.exe" "%INSTALL_DIR%\" >nul
    if %errorLevel% neq 0 (
        echo [警告] 复制 wind_setting.exe 失败
    ) else (
        echo   - wind_setting.exe 已复制
    )
) else (
    echo [提示] 未找到 wind_setting.exe,已跳过(可选)
)

echo [6/10] 从 build 目录复制词库(源文件, wdb 运行时自动生成)...
REM 创建词库目录
if not exist "%INSTALL_DIR%\dict\pinyin" mkdir "%INSTALL_DIR%\dict\pinyin"
if not exist "%INSTALL_DIR%\dict\wubi86" mkdir "%INSTALL_DIR%\dict\wubi86"

REM 拼音词库源文件（Rime YAML 格式）
if exist "%BUILD_DIR%\dict\pinyin\8105.dict.yaml" (
    copy /Y "%BUILD_DIR%\dict\pinyin\8105.dict.yaml" "%INSTALL_DIR%\dict\pinyin\8105.dict.yaml" >nul
    echo   - 拼音单字词库: 8105.dict.yaml
) else (
    echo [警告] build 目录中未找到拼音单字词库,请先运行 build_all.bat
)
if exist "%BUILD_DIR%\dict\pinyin\base.dict.yaml" (
    copy /Y "%BUILD_DIR%\dict\pinyin\base.dict.yaml" "%INSTALL_DIR%\dict\pinyin\base.dict.yaml" >nul
    echo   - 拼音基础词库: base.dict.yaml
) else (
    echo [警告] build 目录中未找到拼音基础词库,请先运行 build_all.bat
)

REM Unigram 语言模型源文件
if exist "%BUILD_DIR%\dict\pinyin\unigram.txt" (
    copy /Y "%BUILD_DIR%\dict\pinyin\unigram.txt" "%INSTALL_DIR%\dict\pinyin\unigram.txt" >nul
    echo   - 语言模型: unigram.txt
) else (
    echo [提示] Unigram 语言模型不存在,智能组句功能不可用
)

REM 五笔码表
if exist "%BUILD_DIR%\dict\wubi86\wubi86.txt" (
    copy /Y "%BUILD_DIR%\dict\wubi86\wubi86.txt" "%INSTALL_DIR%\dict\wubi86\wubi86.txt" >nul
    echo   - 五笔词库: wubi86.txt
) else (
    echo [警告] build 目录中未找到五笔词库,请先运行 build_all.bat
)

REM 常用字表
if exist "%BUILD_DIR%\dict\common_chars.txt" (
    copy /Y "%BUILD_DIR%\dict\common_chars.txt" "%INSTALL_DIR%\dict\common_chars.txt" >nul
    echo   - 常用字表: common_chars.txt
) else (
    echo [警告] build 目录中未找到常用字表
)

echo [7/12] 复制输入方案配置...
if not exist "%INSTALL_DIR%\schemas" mkdir "%INSTALL_DIR%\schemas"
if exist "%BUILD_DIR%\schemas\*.schema.yaml" (
    copy /Y "%BUILD_DIR%\schemas\*.schema.yaml" "%INSTALL_DIR%\schemas\" >nul
    echo   - 输入方案配置已复制
) else (
    echo [警告] build 目录中未找到输入方案配置
)

echo [8/12] 复制主题文件...
if exist "%BUILD_DIR%\themes" (
    if not exist "%INSTALL_DIR%\themes" mkdir "%INSTALL_DIR%\themes"
    for /D %%d in ("%BUILD_DIR%\themes\*") do (
        if exist "%%d\theme.yaml" (
            if not exist "%INSTALL_DIR%\themes\%%~nd" mkdir "%INSTALL_DIR%\themes\%%~nd"
            copy /Y "%%d\theme.yaml" "%INSTALL_DIR%\themes\%%~nd\theme.yaml" >nul
            echo   - 主题: %%~nd
        )
    )
) else (
    echo [警告] build 目录中未找到主题文件
)

echo [9/12] 注册 COM 组件...
regsvr32 /s "%INSTALL_DIR%\wind_tsf.dll"
if %errorLevel% neq 0 (
    echo [错误] COM 注册失败
    pause
    exit /b 1
)

echo [10/12] 配置开机自启动...
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Run" /v "WindInput" /t REG_SZ /d "\"%INSTALL_DIR%\wind_input.exe\"" /f >nul 2>&1
if %errorLevel% equ 0 (
    echo   - 已添加开机自启动注册表项
) else (
    echo [警告] 添加开机自启动失败
)

echo [11/12] 预启动输入法服务...
start "" "%INSTALL_DIR%\wind_input.exe"
echo   - 服务已在后台启动

echo [12/12] 创建快捷方式...
REM 创建开始菜单快捷方式
if exist "%INSTALL_DIR%\wind_setting.exe" (
    powershell -Command "$ws = New-Object -ComObject WScript.Shell; $s = $ws.CreateShortcut('%ProgramData%\Microsoft\Windows\Start Menu\Programs\清风输入法 设置.lnk'); $s.TargetPath = '%INSTALL_DIR%\wind_setting.exe'; $s.WorkingDirectory = '%INSTALL_DIR%'; $s.Description = '清风输入法 设置'; $s.Save()" >nul 2>&1
    if %errorLevel% equ 0 (
        echo   - 开始菜单快捷方式已创建
    )
)

REM 清理旧备份文件(尝试删除 .old_* 和 .bak)
echo.
echo 正在清理旧备份文件...
for %%f in ("%INSTALL_DIR%\*.old_*") do (
    del /F "%%f" >nul 2>&1
)
for %%f in ("%INSTALL_DIR%\*.bak") do (
    del /F "%%f" >nul 2>&1
)

echo.
echo ======================================
echo 安装完成！
echo ======================================
echo.
echo 安装目录: %INSTALL_DIR%
echo.
echo 已安装组件:
echo - wind_tsf.dll (TSF 桥接)
echo - wind_dwrite.dll (DirectWrite 渲染 Shim)
echo - wind_input.exe (输入法服务)
echo - wind_setting.exe (设置界面)
echo - dict\pinyin\8105.dict.yaml (拼音单字词库)
echo - dict\pinyin\base.dict.yaml (拼音基础词库)
echo - dict\pinyin\unigram.txt (语言模型)
echo - dict\wubi86\wubi86.txt (五笔86词库)
echo - dict\common_chars.txt (常用字表)
echo - schemas\*.schema.yaml (输入方案配置)
echo - themes\*\theme.yaml (主题配置)
echo.
echo 服务已自动启动，并已配置开机自启动。
echo.
echo 使用方法:
echo 1. 按 Win+Space 切换输入法
echo 2. 从输入法列表选择“清风输入法”
echo 3. 开始输入(默认拼音模式)
echo.
echo 热键:
echo - Shift: 切换中英文模式
echo - Ctrl+`: 切换拼音/五笔引擎
echo.
echo 设置:
echo - 运行 wind_setting.exe 或在开始菜单中找到“清风输入法 设置”
echo - 配置位置: %%APPDATA%%\WindInput\config.yaml
echo.
echo 注意: 如果旧文件无法删除,请重启电脑后
echo 重新运行安装程序以完成清理。
echo.

