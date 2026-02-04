@echo off
setlocal

echo ======================================
echo WindInput IME Uninstaller
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

set "INSTALL_DIR=%ProgramW6432%\WindInput"
if "%ProgramW6432%"=="" set "INSTALL_DIR=%ProgramFiles%\WindInput"

echo [1/5] Stopping service...
taskkill /F /IM wind_input.exe >nul 2>&1
timeout /t 2 /nobreak >nul

echo [2/5] Unregistering COM component...
if exist "%INSTALL_DIR%\wind_tsf.dll" (
    regsvr32 /u /s "%INSTALL_DIR%\wind_tsf.dll"
)

REM Also try to unregister any old backup DLLs
for %%f in ("%INSTALL_DIR%\wind_tsf*.dll") do (
    regsvr32 /u /s "%%f" >nul 2>&1
)

echo [3/5] Deleting files...

REM Generate random suffix for renaming locked files
set "RANDOM_SUFFIX=%RANDOM%%RANDOM%"

REM Try to delete the main DLL
if exist "%INSTALL_DIR%\wind_tsf.dll" (
    del /F "%INSTALL_DIR%\wind_tsf.dll" >nul 2>&1
    if exist "%INSTALL_DIR%\wind_tsf.dll" (
        echo [WARN] Cannot delete wind_tsf.dll, renaming for later cleanup
        ren "%INSTALL_DIR%\wind_tsf.dll" "wind_tsf.dll.old_%RANDOM_SUFFIX%" >nul 2>&1
    )
)

REM Try to delete wind_input.exe
if exist "%INSTALL_DIR%\wind_input.exe" (
    del /F "%INSTALL_DIR%\wind_input.exe" >nul 2>&1
    if exist "%INSTALL_DIR%\wind_input.exe" (
        echo [WARN] Cannot delete wind_input.exe, renaming for later cleanup
        ren "%INSTALL_DIR%\wind_input.exe" "wind_input.exe.old_%RANDOM_SUFFIX%" >nul 2>&1
    )
)

echo [4/5] Cleaning up backup files...
REM Try to delete all .old_* and .bak files
for %%f in ("%INSTALL_DIR%\*.old_*") do (
    del /F "%%f" >nul 2>&1
)
for %%f in ("%INSTALL_DIR%\*.bak") do (
    del /F "%%f" >nul 2>&1
)

echo [5/5] Removing directory...
REM Remove Start Menu shortcut
del /F "%ProgramData%\Microsoft\Windows\Start Menu\Programs\WindInput Settings.lnk" >nul 2>&1
REM Delete dict folder
if exist "%INSTALL_DIR%\dict" (
    rmdir /S /Q "%INSTALL_DIR%\dict" >nul 2>&1
)

REM Try to remove the install directory (will fail if files remain)
rmdir /S /Q "%INSTALL_DIR%" >nul 2>&1

REM Check if directory still exists
if exist "%INSTALL_DIR%" (
    echo.
    echo [WARN] Some files could not be deleted and have been renamed.
    echo        They will be cleaned up after a restart.
    echo.
    echo Remaining files:
    dir /B "%INSTALL_DIR%" 2>nul
)

echo.
echo ======================================
echo Uninstallation Complete!
echo ======================================
echo.
echo Note: If some files could not be deleted:
echo 1. Restart your computer
echo 2. Manually delete: %INSTALL_DIR%
echo.
echo If the input method still appears in the list,
echo please log out or restart your computer.
echo.

