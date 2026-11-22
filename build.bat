@echo off
set HTTP_PROXY=
set HTTPS_PROXY=
taskkill /F /IM prevhost.exe >nul 2>&1
dotnet publish "%~dp0MediaPreviewPane.csproj" -c Release -r win-x64 -o "%~dp0dist"
if errorlevel 1 pause & exit /b 1
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0Scripts\register.ps1"
pause