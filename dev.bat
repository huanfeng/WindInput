@echo off
setlocal

set "SCRIPT_DIR=%~dp0"
set "CHOICE=%~1"

if "%CHOICE%"=="" goto menu
goto run

:menu
echo ======================================
echo WindInput - Dev Menu
echo ======================================
echo 1. 卸载 / 构建(Release) / 安装
echo 2. 卸载 / 构建(Debug) / 安装
echo 3. 构建(Release)
echo 4. 构建(Debug)
echo 5. 安装
echo 6. 卸载
echo 7. 卸载 / 安装
echo 8. 生成安装包(Release)
echo 9. 生成安装包(跳过编译)
echo.
set /p CHOICE=请选择 (1-9):
echo.
if "%CHOICE%"=="" exit /b 1

:run
if "%CHOICE%"=="1" goto combo_1
if "%CHOICE%"=="2" goto combo_2
if "%CHOICE%"=="3" goto combo_3
if "%CHOICE%"=="4" goto combo_4
if "%CHOICE%"=="5" goto combo_5
if "%CHOICE%"=="6" goto combo_6
if "%CHOICE%"=="7" goto combo_7
if "%CHOICE%"=="8" goto combo_8
if "%CHOICE%"=="9" goto combo_9

echo [ERROR] 无效选项: %CHOICE%
exit /b 1

:combo_1
call :EnsureAdmin %*
if errorlevel 1 exit /b 0
call :DoUninstall || exit /b 1
call :DoBuildRelease || exit /b 1
call :DoInstall || exit /b 1
exit /b 0

:combo_2
call :EnsureAdmin %*
if errorlevel 1 exit /b 0
call :DoUninstall || exit /b 1
call :DoBuildDebug || exit /b 1
call :DoInstall || exit /b 1
exit /b 0

:combo_3
call :DoBuildRelease || exit /b 1
exit /b 0

:combo_4
call :DoBuildDebug || exit /b 1
exit /b 0

:combo_5
call :EnsureAdmin %*
if errorlevel 1 exit /b 0
call :DoInstall || exit /b 1
exit /b 0

:combo_6
call :EnsureAdmin %*
if errorlevel 1 exit /b 0
call :DoUninstall || exit /b 1
exit /b 0

:combo_7
call :EnsureAdmin %*
if errorlevel 1 exit /b 0
call :DoUninstall || exit /b 1
call :DoInstall || exit /b 1
exit /b 0

:combo_8
call :DoBuildInstaller || exit /b 1
exit /b 0

:combo_9
call :DoBuildInstallerSkip || exit /b 1
exit /b 0

:EnsureAdmin
net session >nul 2>&1
if %errorLevel% neq 0 (
    echo [INFO] 需要管理员权限，正在请求提升...
    powershell -Command "Start-Process -FilePath '%~f0' -ArgumentList '%*' -Verb RunAs"
    exit /b 1
)
exit /b 0

:DoBuildRelease
call "%SCRIPT_DIR%build_all.bat" -wails-release
if %errorLevel% neq 0 exit /b 1
exit /b 0

:DoBuildDebug
call "%SCRIPT_DIR%build_all.bat" -wails-debug
if %errorLevel% neq 0 exit /b 1
exit /b 0

:DoInstall
call "%SCRIPT_DIR%installer\install.bat"
if %errorLevel% neq 0 exit /b 1
exit /b 0

:DoUninstall
call "%SCRIPT_DIR%installer\uninstall.bat"
if %errorLevel% neq 0 exit /b 1
exit /b 0

:DoBuildInstaller
call "%SCRIPT_DIR%installer\build_nsis.bat"
if %errorLevel% neq 0 exit /b 1
exit /b 0

:DoBuildInstallerSkip
call "%SCRIPT_DIR%installer\build_nsis.bat" --skip-build
if %errorLevel% neq 0 exit /b 1
exit /b 0
