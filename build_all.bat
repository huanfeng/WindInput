@echo off
setlocal enabledelayedexpansion

echo ======================================
echo WindInput - Build All
echo ======================================
echo.

REM Get script directory
set SCRIPT_DIR=%~dp0

echo [1/5] Building Go service (wind_input.exe)...
cd "%SCRIPT_DIR%wind_input"
go build -o ../build/wind_input.exe ./cmd/service
if %errorLevel% neq 0 (
    echo [ERROR] Go build failed
    pause
    exit /b 1
)
echo Go service built successfully
echo.

echo [2/5] Building C++ DLL (wind_tsf.dll)...
cd "%SCRIPT_DIR%wind_tsf\build"
if not exist "%SCRIPT_DIR%wind_tsf\build" (
    mkdir "%SCRIPT_DIR%wind_tsf\build"
    cd "%SCRIPT_DIR%wind_tsf\build"
    cmake ..
)
cmake --build . --config Release
if %errorLevel% neq 0 (
    echo [ERROR] C++ build failed
    pause
    exit /b 1
)
echo C++ DLL built successfully
echo.

echo [3/5] Building Settings UI (wind_setting.exe)...
cd "%SCRIPT_DIR%wind_setting"
REM Check if wails is installed
where wails >nul 2>&1
if %errorLevel% neq 0 (
    echo [WARN] Wails CLI not found, skipping wind_setting build
    echo        Install with: go install github.com/wailsapp/wails/v2/cmd/wails@latest
) else (
    REM Build with debug mode for DevTools support
    wails build -debug
    if %errorLevel% neq 0 (
        echo [WARN] wind_setting build failed, continuing...
    ) else (
        copy /Y "%SCRIPT_DIR%wind_setting\build\bin\wind_setting.exe" "%SCRIPT_DIR%build\" >nul
        echo Settings UI built successfully (debug mode, press F12 for DevTools)
    )
)
echo.

echo [4/5] Copying dictionaries to build directory...
cd "%SCRIPT_DIR%"
if not exist "%SCRIPT_DIR%build\dict\pinyin" mkdir "%SCRIPT_DIR%build\dict\pinyin"
if not exist "%SCRIPT_DIR%build\dict\wubi" mkdir "%SCRIPT_DIR%build\dict\wubi"

REM Copy pinyin dictionary
if exist "%SCRIPT_DIR%ref\简全拼音库5.0.txt" (
    copy /Y "%SCRIPT_DIR%ref\简全拼音库5.0.txt" "%SCRIPT_DIR%build\dict\pinyin\pinyin.txt" >nul
    echo   - Pinyin dictionary copied
) else (
    echo [WARN] Pinyin dictionary not found in ref directory
)

REM Copy wubi dictionary
if exist "%SCRIPT_DIR%ref\极爽词库6.txt" (
    copy /Y "%SCRIPT_DIR%ref\极爽词库6.txt" "%SCRIPT_DIR%build\dict\wubi\wubi86.txt" >nul
    echo   - Wubi dictionary copied
) else (
    echo [WARN] Wubi dictionary not found in ref directory
)

REM Copy common chars table
if exist "%SCRIPT_DIR%dict\common_chars.txt" (
    copy /Y "%SCRIPT_DIR%dict\common_chars.txt" "%SCRIPT_DIR%build\dict\common_chars.txt" >nul
    echo   - Common chars table copied
) else (
    echo [WARN] Common chars table not found
)
echo.

echo [5/5] Checking output files...
if not exist "%SCRIPT_DIR%build\wind_tsf.dll" (
    echo [ERROR] wind_tsf.dll not found
    pause
    exit /b 1
)

if not exist "%SCRIPT_DIR%build\wind_input.exe" (
    echo [ERROR] wind_input.exe not found
    pause
    exit /b 1
)

echo.
echo ======================================
echo Build Complete!
echo ======================================
echo.
echo Output files:
echo - build\wind_tsf.dll (TSF Bridge)
echo - build\wind_input.exe (IME Service with Native UI)
echo - build\wind_setting.exe (Settings UI, optional)
echo - build\dict\pinyin\pinyin.txt (Pinyin dictionary)
echo - build\dict\wubi\wubi86.txt (Wubi dictionary)
echo - build\dict\common_chars.txt (Common chars table)
echo.
echo For development:
echo   cd build ^&^& wind_input.exe -log debug
echo.
echo For installation:
echo   Run installer\install.bat as Administrator
echo.

