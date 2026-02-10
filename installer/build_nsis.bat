@echo off
setlocal EnableExtensions

set "SCRIPT_DIR=%~dp0"
for %%I in ("%SCRIPT_DIR%..") do set "ROOT_DIR=%%~fI"

set "APP_VERSION=0.1.0-dev"
set "SKIP_BUILD=0"
set "APP_VERSION_NUM=0.1.0.0"

if not "%~1"=="" (
    if /I "%~1"=="--skip-build" (
        set "SKIP_BUILD=1"
    ) else (
        set "APP_VERSION=%~1"
    )
)

if not "%~2"=="" (
    if /I "%~2"=="--skip-build" set "SKIP_BUILD=1"
)

for /f "tokens=1 delims=-+" %%A in ("%APP_VERSION%") do set "VERSION_CORE=%%A"
set "V1=0"
set "V2=0"
set "V3=0"
set "V4=0"
for /f "tokens=1-4 delims=." %%A in ("%VERSION_CORE%") do (
    if not "%%A"=="" set "V1=%%A"
    if not "%%B"=="" set "V2=%%B"
    if not "%%C"=="" set "V3=%%C"
    if not "%%D"=="" set "V4=%%D"
)
set "APP_VERSION_NUM=%V1%.%V2%.%V3%.%V4%"

echo ======================================
echo 清风输入法 NSIS Installer Builder
echo ======================================
echo Version: %APP_VERSION%
echo Version(Numeric): %APP_VERSION_NUM%
echo.

if "%SKIP_BUILD%"=="0" (
    echo [1/3] Build release artifacts...
    call "%ROOT_DIR%\build_all.bat" -wails-release
    if errorlevel 1 (
        echo [ERROR] build_all.bat failed.
        exit /b 1
    )
) else (
    echo [1/3] Skip build stage.
)

echo [2/3] Check makensis...
where makensis >nul 2>&1
if errorlevel 1 (
    echo [ERROR] makensis not found in PATH.
    echo Please install NSIS and ensure makensis.exe is in PATH.
    exit /b 1
)

if not exist "%ROOT_DIR%\build\installer" mkdir "%ROOT_DIR%\build\installer"

echo [3/3] Build installer...
pushd "%ROOT_DIR%\installer\nsis"
makensis /DAPP_VERSION=%APP_VERSION% /DAPP_VERSION_NUM=%APP_VERSION_NUM% WindInput.nsi
if errorlevel 1 (
    popd
    echo [ERROR] NSIS build failed.
    exit /b 1
)
popd

echo.
echo ======================================
echo Installer build completed.
echo Output: build\installer\清风输入法-%APP_VERSION%-Setup.exe
echo ======================================
echo.

exit /b 0
