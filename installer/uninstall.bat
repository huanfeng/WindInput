@echo off
setlocal

echo ======================================
echo WindInput 输入法卸载程序
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

set "INSTALL_DIR=%ProgramW6432%\WindInput"
if "%ProgramW6432%"=="" set "INSTALL_DIR=%ProgramFiles%\WindInput"

echo [1/5] 停止服务...
taskkill /F /IM wind_input.exe >nul 2>&1
timeout /t 2 /nobreak >nul

echo [2/5] 注销 COM 组件...
if exist "%INSTALL_DIR%\wind_tsf.dll" (
    regsvr32 /u /s "%INSTALL_DIR%\wind_tsf.dll"
)

REM 同时尝试注销旧备份 DLL
for %%f in ("%INSTALL_DIR%\wind_tsf*.dll") do (
    regsvr32 /u /s "%%f" >nul 2>&1
)

echo [3/5] 删除文件...

REM 生成随机后缀用于重命名被占用文件
set "RANDOM_SUFFIX=%RANDOM%%RANDOM%"

REM 尝试删除主 DLL
if exist "%INSTALL_DIR%\wind_tsf.dll" (
    del /F "%INSTALL_DIR%\wind_tsf.dll" >nul 2>&1
    if exist "%INSTALL_DIR%\wind_tsf.dll" (
        echo [警告] 无法删除 wind_tsf.dll,重命名以便稍后清理
        ren "%INSTALL_DIR%\wind_tsf.dll" "wind_tsf.dll.old_%RANDOM_SUFFIX%" >nul 2>&1
    )
)

REM 尝试删除 wind_input.exe
if exist "%INSTALL_DIR%\wind_input.exe" (
    del /F "%INSTALL_DIR%\wind_input.exe" >nul 2>&1
    if exist "%INSTALL_DIR%\wind_input.exe" (
        echo [警告] 无法删除 wind_input.exe,重命名以便稍后清理
        ren "%INSTALL_DIR%\wind_input.exe" "wind_input.exe.old_%RANDOM_SUFFIX%" >nul 2>&1
    )
)

echo [4/5] 清理备份文件...
REM 尝试删除所有 .old_* 和 .bak 文件
for %%f in ("%INSTALL_DIR%\*.old_*") do (
    del /F "%%f" >nul 2>&1
)
for %%f in ("%INSTALL_DIR%\*.bak") do (
    del /F "%%f" >nul 2>&1
)

echo [5/5] 移除目录...
REM 删除开始菜单快捷方式
del /F "%ProgramData%\Microsoft\Windows\Start Menu\Programs\WindInput 设置.lnk" >nul 2>&1
REM 删除词库目录
if exist "%INSTALL_DIR%\dict" (
    rmdir /S /Q "%INSTALL_DIR%\dict" >nul 2>&1
)

REM 尝试删除安装目录(如有残留文件会失败)
rmdir /S /Q "%INSTALL_DIR%" >nul 2>&1

REM 检查目录是否仍然存在
if exist "%INSTALL_DIR%" (
    echo.
    echo [警告] 部分文件无法删除,已重命名。
    echo        重启后可完成清理。
    echo.
    echo 剩余文件:
    dir /B "%INSTALL_DIR%" 2>nul
)

echo.
echo ======================================
echo 卸载完成！
echo ======================================
echo.
echo 注意: 如果仍有文件无法删除:
echo 1. 重启电脑
echo 2. 手动删除: %INSTALL_DIR%
echo.
echo 如果输入法仍出现在列表中,
echo 请注销或重启电脑。
echo.

