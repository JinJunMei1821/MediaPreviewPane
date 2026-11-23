$ErrorActionPreference = 'Stop'

$clsid = '{9F4E2B61-7A3D-4E8C-9B12-5D8A1E4C7F30}'
$previewIid = '{8895b1c6-b41f-4c1c-a562-0d564250836f}'
$prevhostAppId = '{6d2b5079-2f0b-48dd-ab7f-97cec514d30b}'

$root = Split-Path -Parent $PSScriptRoot
$distDir = Join-Path $root 'dist'
$dllPath = Join-Path $distDir 'MediaPreviewPane.dll'
if (-not (Test-Path $dllPath)) {
    # Fallback: check root directory (single-DLL release layout)
    $altPath = Join-Path $root 'MediaPreviewPane.dll'
    if (Test-Path $altPath) {
        $dllPath = $altPath
        $distDir = $root
    } else {
        throw "未找到 MediaPreviewPane.dll，请先执行构建或下载 Release 压缩包。"
    }
}

# Remove Mark-of-the-Web (Zone.Identifier) so prevhost.exe can load downloaded DLLs cleanly
Get-ChildItem -Path $distDir -Filter '*.dll' -ErrorAction SilentlyContinue | Unblock-File -ErrorAction SilentlyContinue

# Stop stale preview hosts so updated binaries take effect immediately
Get-Process -Name prevhost -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue

$extensions = @(
    '.ogg', '.oga', '.opus', '.mp3', '.wav', '.flac', '.m4a', '.aac', '.weba', '.aiff',
    '.mp4', '.m4v', '.webm', '.mov', '.ogv', '.mkv', '.wmv'
)

# 1. Register COM CLSID under HKCU (no admin required)
$clsidKey = "HKCU:\Software\Classes\CLSID\$clsid"
$inprocKey = "$clsidKey\InprocServer32"
New-Item -Path $inprocKey -Force | Out-Null
Set-ItemProperty -Path $clsidKey -Name '(default)' -Value 'MediaPreviewPaneHandler'
Set-ItemProperty -Path $clsidKey -Name 'DisplayName' -Value 'Media Preview Pane'
Set-ItemProperty -Path $clsidKey -Name 'AppID' -Value $prevhostAppId
Set-ItemProperty -Path $clsidKey -Name 'DisableLowILProcessIsolation' -Value 1 -Type DWord
Set-ItemProperty -Path $inprocKey -Name '(default)' -Value $dllPath
Set-ItemProperty -Path $inprocKey -Name 'ThreadingModel' -Value 'Apartment'

# 2. Register in PreviewHandlers whitelist
$phKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\PreviewHandlers'
New-Item -Path $phKey -Force | Out-Null
Set-ItemProperty -Path $phKey -Name $clsid -Value 'Media Preview Pane'

# 3. Associate supported audio & video extensions
foreach ($ext in $extensions) {
    $extKey = "HKCU:\Software\Classes\$ext\ShellEx\$previewIid"
    New-Item -Path $extKey -Force | Out-Null
    Set-ItemProperty -Path $extKey -Name '(default)' -Value $clsid
}

# 4. Notify Shell to refresh associations immediately
$sig = '[DllImport("shell32.dll")] public static extern void SHChangeNotify(int wEventId, uint uFlags, IntPtr dwItem1, IntPtr dwItem2);'
$type = Add-Type -MemberDefinition $sig -Name 'ShellNotifyReg' -Namespace 'MediaPreviewReg' -PassThru
$type::SHChangeNotify(0x08000000, 0x0000, [IntPtr]::Zero, [IntPtr]::Zero)

Write-Host "MediaPreviewPane (Rust Native) 注册成功！" -ForegroundColor Green
Write-Host "DLL: $dllPath"
Write-Host "已关联格式: $($extensions -join ', ')"