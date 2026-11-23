# MediaPreviewPane

极简、纯原生 Rust 编写的 Windows 资源管理器「预览窗格（`Alt + P`）」音视频试听插件。

- **零外部运行时依赖**：单个约 3 MB 的原生 DLL（静态链接 CRT，无需 .NET、无需 WebView2 / Chromium、无需 VC++ 运行库）
- **极致轻盈**：直接运行在系统 `prevhost.exe` 内，无额外子进程，实测内存约 **16 MB**（私有内存 ~6 MB），冷启动 ~15 ms，切歌 ~3 ms（复访缓存 ~1 ms）
- **零存在感 & 不锁文件**：平时不占任务栏/托盘，仅在选中媒体文件且开启预览窗格时出现；音频与中小视频读入内存后立刻释放句柄，重命名/移动/删除文件绝不卡锁
- **简练现代原生 UI**：Direct2D + DirectWrite 高 DPI 矢量绘制，自动跟随 Windows 深/浅色主题与系统强调色

## 支持格式
- **音频**（内置纯 Rust 解码 + WASAPI 低延迟输出，无需系统解码器）：
  `.ogg` `.oga` `.opus` `.mp3` `.wav` `.flac` `.m4a` `.aac` `.weba` `.aiff`
- **视频**（调用 Windows 原生 Media Foundation 硬件解码）：
  `.mp4` `.m4v` `.mov` `.webm` `.mkv` `.ogv` `.wmv`

## 功能与快捷键
- **双层音频波形（峰值 + RMS）**：点击或拖动任意位置精准跳转，鼠标悬停显示精确时间点；24 MB 以内短视频同时显示音频波形
- **播放控制**：播放/暂停、循环开关、**原调倍速**（`0.5×` `0.75×` `1×` `1.25×` `1.5×` `2×`，右键重置 `1×`，支持滚轮微调）、音量/静音、选中自动播放开关（默认关闭，设置自动记忆）
- **快捷键**：
  - 在资源管理器文件列表选中后，直接按 `Space`（空格）即可试听/暂停（自动避让重命名、搜索框、地址栏）
  - 预览窗格获焦时支持：`Space` 播放/暂停、`←` / `→` 跳转、`L` 循环、`M` 静音、`[` / `]` 调速、`↑` / `↓` 音量

## 安装与卸载（Win10 / Win11 x64，无需管理员权限）
1. 下载 [最新 Release 压缩包](https://github.com/JinJunMei1821/MediaPreviewPane/releases/latest) 并解压到固定目录（例如 `%LOCALAPPDATA%\MediaPreviewPane`）
2. 双击 `register.bat`（或运行 `powershell -NoProfile -ExecutionPolicy Bypass -File .\Scripts\register.ps1`）
3. 打开资源管理器按 `Alt + P` 开启预览窗格，选中任意音视频即可使用（不影响原本双击打开方式）
4. 如需卸载：双击 `unregister.bat`，然后直接删除目录即可

## 从源码构建
需要 Rust（`x86_64-pc-windows-msvc`）：
```bat
build.bat
```

## 给 AI Agent 的自动部署指引
```powershell
$installDir = "$env:LOCALAPPDATA\MediaPreviewPane"
New-Item -ItemType Directory -Force -Path $installDir | Out-Null
$zip = "$env:TEMP\MediaPreviewPane-win-x64.zip"
Invoke-WebRequest -Uri "https://github.com/JinJunMei1821/MediaPreviewPane/releases/latest/download/MediaPreviewPane-win-x64.zip" -OutFile $zip
Expand-Archive -Path $zip -DestinationPath $installDir -Force
powershell -NoProfile -ExecutionPolicy Bypass -File "$installDir\Scripts\register.ps1"
```