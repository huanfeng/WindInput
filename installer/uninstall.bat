@echo off
net session >nul 2>&1
if %errorLevel% neq 0 (
    echo [INFO] 需要管理员权限，正在请求提升...
    powershell -Command "Start-Process cmd -Verb RunAs -ArgumentList '/c \"\"%~f0\"\"'"
    exit /b 0
)
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0uninstall.ps1" %*
pause
