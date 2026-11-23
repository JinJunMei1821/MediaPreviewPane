@echo off
setlocal
cd /d "%~dp0"
echo Building MediaPreviewPane (Rust native x64)...
cargo build --release
if errorlevel 1 (
    echo Build failed!
    pause
    exit /b 1
)
if not exist "%~dp0dist" mkdir "%~dp0dist"
taskkill /F /IM prevhost.exe >nul 2>&1
copy /Y "%~dp0target\release\MediaPreviewPane.dll" "%~dp0dist\MediaPreviewPane.dll" >nul
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0Scripts\register.ps1"
echo Done!
pause