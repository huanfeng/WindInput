@echo off
setlocal

echo ======================================
echo WindInput IME Installer
echo ======================================
echo.

REM Check administrator privileges
net session >nul 2>&1
if %errorLevel% neq 0 (
    echo [ERROR] Please run this script as Administrator!
    echo Right-click this file and select "Run as administrator"
    pause
    exit /b 1
)

REM Get script directory
set SCRIPT_DIR=%~dp0
set BUILD_DIR=%SCRIPT_DIR%..\build

echo [1/8] Checking files...
if not exist "%BUILD_DIR%\wind_tsf.dll" (
    echo [ERROR] wind_tsf.dll not found
    echo Please run build_all.bat first
    pause
    exit /b 1
)

if not exist "%BUILD_DIR%\wind_input.exe" (
    echo [ERROR] wind_input.exe not found
    echo Please run build_all.bat first
    pause
    exit /b 1
)

echo [2/8] Stopping old processes...
taskkill /F /IM wind_input.exe >nul 2>&1
timeout /t 1 /nobreak >nul

echo [3/8] Creating install directory...
echo [4/8] Handling existing files...
set "INSTALL_DIR=%ProgramW6432%\WindInput"
if "%ProgramW6432%"=="" set "INSTALL_DIR=%ProgramFiles%\WindInput"
if not exist "%INSTALL_DIR%" mkdir "%INSTALL_DIR%"

REM Generate random suffix
set "RANDOM_SUFFIX=%RANDOM%%RANDOM%"

REM Handle old DLL - try to delete, if fails rename it
if exist "%INSTALL_DIR%\wind_tsf.dll" (
    REM First unregister the old DLL
    regsvr32 /u /s "%INSTALL_DIR%\wind_tsf.dll" >nul 2>&1

    del /F "%INSTALL_DIR%\wind_tsf.dll" >nul 2>&1
    if exist "%INSTALL_DIR%\wind_tsf.dll" (
        echo [WARN] Cannot delete old wind_tsf.dll, renaming to wind_tsf.dll.old_%RANDOM_SUFFIX%
        ren "%INSTALL_DIR%\wind_tsf.dll" "wind_tsf.dll.old_%RANDOM_SUFFIX%" >nul 2>&1
        if exist "%INSTALL_DIR%\wind_tsf.dll" (
            echo [WARN] Cannot rename either, trying alternative name...
            ren "%INSTALL_DIR%\wind_tsf.dll" "wind_tsf_%RANDOM_SUFFIX%.dll.bak" >nul 2>&1
        )
    )
)

REM Handle old wind_input.exe
if exist "%INSTALL_DIR%\wind_input.exe" (
    del /F "%INSTALL_DIR%\wind_input.exe" >nul 2>&1
    if exist "%INSTALL_DIR%\wind_input.exe" (
        echo [WARN] Cannot delete old wind_input.exe, renaming to wind_input.exe.old_%RANDOM_SUFFIX%
        ren "%INSTALL_DIR%\wind_input.exe" "wind_input.exe.old_%RANDOM_SUFFIX%" >nul 2>&1
    )
)

REM Handle old wind_setting.exe
if exist "%INSTALL_DIR%\wind_setting.exe" (
    del /F "%INSTALL_DIR%\wind_setting.exe" >nul 2>&1
    if exist "%INSTALL_DIR%\wind_setting.exe" (
        echo [WARN] Cannot delete old wind_setting.exe, renaming
        ren "%INSTALL_DIR%\wind_setting.exe" "wind_setting.exe.old_%RANDOM_SUFFIX%" >nul 2>&1
    )
)

echo [5/8] Copying files...
copy /Y "%BUILD_DIR%\wind_tsf.dll" "%INSTALL_DIR%\" >nul
if %errorLevel% neq 0 (
    echo [ERROR] Failed to copy wind_tsf.dll
    pause
    exit /b 1
)

copy /Y "%BUILD_DIR%\wind_input.exe" "%INSTALL_DIR%\" >nul
if %errorLevel% neq 0 (
    echo [ERROR] Failed to copy wind_input.exe
    pause
    exit /b 1
)

REM Copy wind_setting.exe (optional)
if exist "%BUILD_DIR%\wind_setting.exe" (
    copy /Y "%BUILD_DIR%\wind_setting.exe" "%INSTALL_DIR%\" >nul
    if %errorLevel% neq 0 (
        echo [WARN] Failed to copy wind_setting.exe
    ) else (
        echo   - wind_setting.exe copied
    )
) else (
    echo [INFO] wind_setting.exe not found, skipping (optional)
)

echo [6/8] Copying dictionaries from build directory...
REM Create dict directories
if not exist "%INSTALL_DIR%\dict\pinyin" mkdir "%INSTALL_DIR%\dict\pinyin"
if not exist "%INSTALL_DIR%\dict\wubi" mkdir "%INSTALL_DIR%\dict\wubi"

REM Copy pinyin dictionary from build
if exist "%BUILD_DIR%\dict\pinyin\pinyin.txt" (
    copy /Y "%BUILD_DIR%\dict\pinyin\pinyin.txt" "%INSTALL_DIR%\dict\pinyin\pinyin.txt" >nul
    echo   - Pinyin dictionary: pinyin.txt
) else (
    echo [WARN] Pinyin dictionary not found in build directory
    echo        Please run build_all.bat first
)

REM Copy wubi dictionary from build
if exist "%BUILD_DIR%\dict\wubi\wubi86.txt" (
    copy /Y "%BUILD_DIR%\dict\wubi\wubi86.txt" "%INSTALL_DIR%\dict\wubi\wubi86.txt" >nul
    echo   - Wubi dictionary: wubi86.txt
) else (
    echo [WARN] Wubi dictionary not found in build directory
    echo        Please run build_all.bat first
)

REM Copy common chars table from build
if exist "%BUILD_DIR%\dict\common_chars.txt" (
    copy /Y "%BUILD_DIR%\dict\common_chars.txt" "%INSTALL_DIR%\dict\common_chars.txt" >nul
    echo   - Common chars table: common_chars.txt
) else (
    echo [WARN] Common chars table not found in build directory
)

echo [7/8] Registering COM component...
regsvr32 /s "%INSTALL_DIR%\wind_tsf.dll"
if %errorLevel% neq 0 (
    echo [ERROR] COM registration failed
    pause
    exit /b 1
)

echo [8/8] Creating shortcuts...
REM Create Start Menu shortcut for Settings
if exist "%INSTALL_DIR%\wind_setting.exe" (
    powershell -Command "$ws = New-Object -ComObject WScript.Shell; $s = $ws.CreateShortcut('%ProgramData%\Microsoft\Windows\Start Menu\Programs\WindInput Settings.lnk'); $s.TargetPath = '%INSTALL_DIR%\wind_setting.exe'; $s.WorkingDirectory = '%INSTALL_DIR%'; $s.Description = 'WindInput Settings'; $s.Save()" >nul 2>&1
    if %errorLevel% equ 0 (
        echo   - Start Menu shortcut created
    )
)

REM Clean up old backup files (try to delete .old_* and .bak files)
echo.
echo Cleaning up old backup files...
for %%f in ("%INSTALL_DIR%\*.old_*") do (
    del /F "%%f" >nul 2>&1
)
for %%f in ("%INSTALL_DIR%\*.bak") do (
    del /F "%%f" >nul 2>&1
)

echo.
echo ======================================
echo Installation Complete!
echo ======================================
echo.
echo Install directory: %INSTALL_DIR%
echo.
echo Components installed:
echo - wind_tsf.dll (TSF Bridge)
echo - wind_input.exe (IME Service)
echo - wind_setting.exe (Settings UI)
echo - dict\pinyin\pinyin.txt (Pinyin dictionary)
echo - dict\wubi\wubi86.txt (Wubi86 dictionary)
echo - dict\common_chars.txt (Common chars table)
echo.
echo The service will start automatically when you use the IME.
echo.
echo Usage:
echo 1. Press Win+Space to switch input method
echo 2. Select "WindInput" from the input method list
echo 3. Start typing (default: Pinyin mode)
echo.
echo Hotkeys:
echo - Shift: Toggle Chinese/English mode
echo - Ctrl+`: Switch between Pinyin and Wubi engine
echo.
echo Settings:
echo - Run wind_setting.exe or find "WindInput Settings" in Start Menu
echo - Config location: %%APPDATA%%\WindInput\config.yaml
echo.
echo Note: If old files could not be deleted, restart your
echo computer and run this installer again to clean them up.
echo.
pause
