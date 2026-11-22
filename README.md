# MediaPreviewPane

Windows 资源管理器「预览窗格（`Alt + P`）」音视频预览插件（`IPreviewHandler`）。

无需打开任何独立软件，平时不占任务栏/托盘；仅在资源管理器选中音频或视频文件且开启预览窗格时出现，切走即停，不锁定磁盘文件（预览中可正常重命名/移动/删除）。

## 功能特性
- **音频波形线（Waveform）**：点击或拖动波形任意位置精准跳转，悬停显示时间点
- **视频预览**：选中即显示画面帧，点击画面或按播放键播放（25 MB 以内短视频同时显示音频波形）
- **播放控制**：播放/暂停（带清晰状态提示）、循环/单次切换、倍速（`0.5x` ~ `2.0x`，保持原调）、进度条、音量/静音、可选「选中自动播放」（默认关闭，设置自动记住）
- **快捷键**：
  - 在资源管理器选中文件后直接按 **`Space`（空格）** 播放/暂停（重命名、搜索框、地址栏输入时自动避让）
  - 预览窗格内支持 `Space`（播放/暂停）、`←` / `→`（步进跳转）、`L`（循环）、`M`（静音）
- **支持格式**：
  - 音频：`.ogg` `.oga` `.opus` `.mp3` `.wav` `.flac` `.m4a` `.aac` `.weba`
  - 视频：`.mp4` `.m4v` `.webm` `.mov` `.ogv` `.mkv`

## 系统兼容性与依赖
- **支持系统**：Windows 10 / Windows 11 (x64)
- **运行依赖**：
  1. **.NET 9.0 Desktop Runtime (x64)**（因 .NET COM Hosting 不支持自包含打包，需系统已安装对应 Desktop Runtime，可用 `winget install --id Microsoft.DotNet.DesktopRuntime.9 --exact` 一键安装）
  2. **Microsoft Edge WebView2 Runtime**（Windows 10 / 11 通常已自带）
- **Windows 11 注意事项**：
  - 若当前右侧打开的是「详细信息窗格」，请按 `Alt + P` 切换为「预览窗格」。
  - Windows 11 新版安全更新会出于系统安全策略默认拦截「从互联网下载且带有安全标记（MOTW）」的媒体文件预览；右键该媒体文件 → 属性 → 勾选「解除锁定」即可正常预览（本地文件不受影响）。
  - 注册脚本已内置 `Unblock-File`，会自动解除插件 DLL 自身的下载锁定。

## 快速安装

### 方式一：下载预编译包（推荐，约 1 MB）
1. 确保已安装 **.NET 9.0 Desktop Runtime (x64)**：
   ```powershell
   winget install --id Microsoft.DotNet.DesktopRuntime.9 --exact
   ```
2. 前往 [Releases](https://github.com/JinJunMei1821/MediaPreviewPane/releases/latest) 下载 `MediaPreviewPane-win-x64.zip`。
3. 解压到一个**固定目录**（例如 `%LOCALAPPDATA%\MediaPreviewPane` 或 `D:\Tools\MediaPreviewPane`，注册后请勿随意移动或删除）。
4. 双击运行 `register.bat`（仅写入当前用户 `HKCU`，**无需管理员权限**，不影响双击默认播放器）。
5. 打开资源管理器按 `Alt + P` 开启预览窗格，点选任意音视频文件即可使用。

### 方式二：从源码编译安装（需 .NET 9 SDK）
```powershell
git clone https://github.com/JinJunMei1821/MediaPreviewPane.git
cd MediaPreviewPane
.\build.bat
```

### 卸载
运行目录内的 `unregister.bat` 即可彻底清理注册表并结束 `prevhost.exe`，之后直接删除整个文件夹即可。

---

## AI Agent 自动部署指引

如果你是 AI Agent，请按以下步骤为用户自动安装并验证本插件：

1. **检查系统与运行库**：
   - 确认当前为 Windows x64。
   - 检查是否已安装 **.NET 9.0 Desktop Runtime (x64)**：
     ```powershell
     Test-Path "$env:ProgramFiles\dotnet\shared\Microsoft.WindowsDesktop.App\9.*"
     ```
     若返回 `False`，安装它（优先使用 winget）：
     ```powershell
     winget install --id Microsoft.DotNet.DesktopRuntime.9 --exact --accept-source-agreements --accept-package-agreements
     ```
   - 确认 Edge WebView2 Runtime 存在（Win10/Win11 通常已预装）。
2. **准备固定安装目录**：
   - 默认使用 `$installDir = Join-Path $env:LOCALAPPDATA 'MediaPreviewPane'`（不要放在临时目录）。
3. **获取程序文件（优先下载 Release，失败则源码构建）**：
   - 下载最新 Release 预编译包并解压到 `$installDir`：
     `https://github.com/JinJunMei1821/MediaPreviewPane/releases/latest/download/MediaPreviewPane-win-x64.zip`
   - 确认存在 `$installDir\dist\MediaPreviewPane.comhost.dll` 和 `$installDir\Scripts\register.ps1`。
   - 若无法访问 Release 资产但本机有 `.NET 9 SDK`，可改为：
     ```powershell
     git clone https://github.com/JinJunMei1821/MediaPreviewPane.git $installDir
     dotnet publish "$installDir\MediaPreviewPane.csproj" -c Release -r win-x64 --self-contained false -o "$installDir\dist"
     ```
4. **注册插件（无需管理员权限）**：
   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File "$installDir\Scripts\register.ps1"
   ```
5. **检查资源管理器预览设置并验证**：
   - 检查 `HKCU:\Software\Classes\CLSID\{9F4E2B61-7A3D-4E8C-9B12-5D8A1E4C7F30}\InprocServer32` 的默认值指向已存在的 `MediaPreviewPane.comhost.dll`。
   - 检查 `HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced` 中 `ShowPreviewHandlers` 不为 `0`（如果为 `0`，说明系统关闭了“在预览窗格中显示预览处理程序”，提示用户开启或改为 `1`）。
6. **汇报给用户**：
   - 告知安装路径与卸载脚本路径（`$installDir\unregister.bat`）。
   - 告知用法：打开资源管理器按 `Alt + P` 开启预览窗格，点选 `.ogg`/`.mp3`/`.wav`/`.m4a`/`.mp4` 等文件，按 `Space` 或点击播放键即可试听。

## 配置与日志路径
`%USERPROFILE%\AppData\LocalLow\MediaPreviewPane\`
- `settings.json`：自动记住循环、倍速、音量、自动播放开关
- `preview.log`：轻量运行日志