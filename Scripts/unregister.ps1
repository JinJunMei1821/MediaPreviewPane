$ErrorActionPreference = 'Stop'

$clsid = '{9F4E2B61-7A3D-4E8C-9B12-5D8A1E4C7F30}'
$previewIid = '{8895b1c6-b41f-4c1c-a562-0d564250836f}'

$extensions = @(
    '.ogg', '.oga', '.opus', '.mp3', '.wav', '.flac', '.m4a', '.aac', '.weba',
    '.mp4', '.m4v', '.webm', '.mov', '.ogv', '.mkv'
)

foreach ($ext in $extensions) {
    $extKey = "HKCU:\Software\Classes\$ext\ShellEx\$previewIid"
    if (Test-Path $extKey) {
        $val = (Get-ItemProperty -Path $extKey -ErrorAction SilentlyContinue).'(default)'
        if ($val -eq $clsid) {
            Remove-Item -Path $extKey -Force -ErrorAction SilentlyContinue
        }
    }
}

$phKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\PreviewHandlers'
if (Test-Path $phKey) {
    Remove-ItemProperty -Path $phKey -Name $clsid -ErrorAction SilentlyContinue
}

$clsidKey = "HKCU:\Software\Classes\CLSID\$clsid"
if (Test-Path $clsidKey) {
    Remove-Item -Path $clsidKey -Recurse -Force -ErrorAction SilentlyContinue
}

Get-Process -Name prevhost -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue

$sig = '[DllImport("shell32.dll")] public static extern void SHChangeNotify(int wEventId, uint uFlags, IntPtr dwItem1, IntPtr dwItem2);'
$type = Add-Type -MemberDefinition $sig -Name 'ShellNotifyUnreg' -Namespace 'MediaPreviewUnreg' -PassThru
$type::SHChangeNotify(0x08000000, 0x0000, [IntPtr]::Zero, [IntPtr]::Zero)

Write-Host "MediaPreviewPane 已彻底卸载并清理注册表。" -ForegroundColor Yellow