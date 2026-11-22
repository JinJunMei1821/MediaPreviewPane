using System;
using System.Diagnostics;
using System.Drawing;
using System.IO;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;
using System.Windows.Forms;
using Microsoft.Web.WebView2.Core;

namespace MediaPreviewPane;

internal sealed class UserSettings
{
    public bool Loop { get; set; } = false;
    public double Speed { get; set; } = 1.0;
    public double Volume { get; set; } = 0.85;
    public bool Muted { get; set; } = false;
    public bool AutoPlay { get; set; } = false;
}

internal sealed class MediaSource
{
    public const long MaxMemoryCacheBytes = 64L * 1024L * 1024L;

    public string FilePath { get; }
    public string FileName { get; }
    public string Extension { get; }
    public string MimeType { get; }
    public bool IsVideo { get; }
    public long FileSize { get; }
    public string Token { get; }
    public byte[]? MemoryData { get; }

    public MediaSource(string filePath, string token)
    {
        FilePath = filePath;
        FileName = Path.GetFileName(filePath);
        Extension = Path.GetExtension(filePath).ToLowerInvariant();
        (MimeType, IsVideo) = GetMediaInfo(Extension);
        Token = token;

        using var fs = new FileStream(
            filePath,
            FileMode.Open,
            FileAccess.Read,
            FileShare.ReadWrite | FileShare.Delete);

        FileSize = fs.Length;
        if (FileSize <= MaxMemoryCacheBytes)
        {
            var buf = new byte[FileSize];
            int totalRead = 0;
            while (totalRead < buf.Length)
            {
                int read = fs.Read(buf, totalRead, buf.Length - totalRead);
                if (read <= 0) break;
                totalRead += read;
            }
            MemoryData = totalRead == buf.Length ? buf : buf[..totalRead];
        }
    }

    public byte[] ReadRange(long start, int length)
    {
        if (length <= 0) return Array.Empty<byte>();

        if (MemoryData != null)
        {
            int actualStart = (int)Math.Min(start, MemoryData.Length);
            int actualLen = (int)Math.Min(length, MemoryData.Length - actualStart);
            var slice = new byte[actualLen];
            Buffer.BlockCopy(MemoryData, actualStart, slice, 0, actualLen);
            return slice;
        }

        using var fs = new FileStream(
            FilePath,
            FileMode.Open,
            FileAccess.Read,
            FileShare.ReadWrite | FileShare.Delete);

        fs.Seek(start, SeekOrigin.Begin);
        var buf = new byte[length];
        int totalRead = 0;
        while (totalRead < length)
        {
            int read = fs.Read(buf, totalRead, length - totalRead);
            if (read <= 0) break;
            totalRead += read;
        }
        return totalRead == length ? buf : buf[..totalRead];
    }

    private static (string Mime, bool IsVideo) GetMediaInfo(string ext) => ext switch
    {
        ".ogg" or ".oga" => ("audio/ogg", false),
        ".opus" => ("audio/ogg", false),
        ".mp3" => ("audio/mpeg", false),
        ".wav" => ("audio/wav", false),
        ".flac" => ("audio/flac", false),
        ".m4a" => ("audio/mp4", false),
        ".aac" => ("audio/aac", false),
        ".weba" => ("audio/webm", false),
        ".mp4" or ".m4v" or ".mov" => ("video/mp4", true),
        ".webm" => ("video/webm", true),
        ".ogv" => ("video/ogg", true),
        ".mkv" => ("video/webm", true),
        _ => ("application/octet-stream", false),
    };
}

internal sealed class HotkeyMessageWindow : NativeWindow
{
    public const int HotkeyId = 0x51A0;
    private readonly Action _onSpacePressed;

    public HotkeyMessageWindow(Action onSpacePressed)
    {
        _onSpacePressed = onSpacePressed;
        var cp = new CreateParams
        {
            Caption = "MediaPreviewPaneHotkeySink",
            X = 0,
            Y = 0,
            Width = 0,
            Height = 0,
            Style = 0
        };
        CreateHandle(cp);
    }

    protected override void WndProc(ref Message m)
    {
        if (m.Msg == NativeMethods.WM_HOTKEY && m.WParam.ToInt32() == HotkeyId)
        {
            _onSpacePressed();
            return;
        }
        base.WndProc(ref m);
    }
}

internal sealed class PreviewHostManager
{
    private const string VirtualOrigin = "https://mediapreview.local";
    private const string IndexUrl = VirtualOrigin + "/index.html";
    private const int MaxStreamChunkBytes = 2 * 1024 * 1024;

    private static readonly object InstanceLock = new();
    private static PreviewHostManager? _instance;
    private static bool _loaderDllPathConfigured;

    public static PreviewHostManager Instance
    {
        get
        {
            if (_instance != null) return _instance;
            lock (InstanceLock)
            {
                _instance ??= new PreviewHostManager();
                return _instance;
            }
        }
    }

    private readonly string _dataRoot;
    private readonly string _settingsPath;
    private readonly string _logPath;
    private readonly string _htmlContent;

    private readonly ManualResetEventSlim _uiReady = new(false);
    private Form _hostForm = null!;
    private Form _parkingForm = null!;
    private HotkeyMessageWindow _hotkeyWindow = null!;
    private System.Windows.Forms.Timer _hotkeyTimer = null!;
    private IntPtr _hostHandle;

    private CoreWebView2Environment? _env;
    private CoreWebView2Controller? _controller;
    private CoreWebView2? _coreWebView;

    private bool _initStarted;
    private bool _webViewReady;
    private bool _pageReady;
    private bool _isAttached;
    private bool _hotkeyRegistered;
    private long _generation;

    private IntPtr _currentParentHwnd;
    private IntPtr _currentExplorerRootHwnd;
    private RECT _currentRect;
    private MediaSource? _currentMedia;
    private UserSettings _settings;

    private static readonly JsonSerializerOptions JsonOpts = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        PropertyNameCaseInsensitive = true
    };

    private PreviewHostManager()
    {
        _dataRoot = Path.Combine(NativeMethods.GetLocalAppDataLowPath(), "MediaPreviewPane");
        Directory.CreateDirectory(_dataRoot);
        _settingsPath = Path.Combine(_dataRoot, "settings.json");
        _logPath = Path.Combine(_dataRoot, "preview.log");
        _htmlContent = LoadEmbeddedHtml();
        _settings = LoadSettings();

        CleanupStaleWebView2Folders();

        var uiThread = new Thread(RunUiThread)
        {
            Name = "MediaPreviewPane-STA",
            IsBackground = true
        };
        uiThread.SetApartmentState(ApartmentState.STA);
        uiThread.Start();

        _uiReady.Wait(TimeSpan.FromSeconds(10));
        Log($"PreviewHostManager initialized (PID={Environment.ProcessId}).");
    }

    private void CleanupStaleWebView2Folders()
    {
        try
        {
            string wvRoot = Path.Combine(_dataRoot, "WebView2");
            if (!Directory.Exists(wvRoot)) return;
            foreach (string dir in Directory.GetDirectories(wvRoot, "p_*"))
            {
                string name = Path.GetFileName(dir);
                if (int.TryParse(name[2..], out int pid) && pid != Environment.ProcessId)
                {
                    bool alive = false;
                    try
                    {
                        using var p = Process.GetProcessById(pid);
                        alive = !p.HasExited;
                    }
                    catch { }
                    if (!alive)
                    {
                        try { Directory.Delete(dir, true); } catch { }
                    }
                }
            }
        }
        catch { }
    }

    private void RunUiThread()
    {
        try
        {
            NativeMethods.OleInitialize(IntPtr.Zero);
            Application.EnableVisualStyles();
            Application.SetCompatibleTextRenderingDefault(false);

            _parkingForm = new Form
            {
                ShowInTaskbar = false,
                FormBorderStyle = FormBorderStyle.None,
                StartPosition = FormStartPosition.Manual,
                Location = new Point(-32000, -32000),
                Size = new Size(100, 100),
                Opacity = 0
            };
            _ = _parkingForm.Handle;

            // Create _hostForm as a real WinForms Form (same pattern as Microsoft PowerToys!)
            _hostForm = new Form
            {
                ShowInTaskbar = false,
                FormBorderStyle = FormBorderStyle.None,
                ControlBox = false,
                StartPosition = FormStartPosition.Manual,
                Location = new Point(-32000, -32000),
                Size = new Size(300, 300),
                BackColor = SystemColors.Window
            };
            _hostHandle = _hostForm.Handle;

            _hotkeyWindow = new HotkeyMessageWindow(OnSpaceHotkey);
            _hotkeyTimer = new System.Windows.Forms.Timer { Interval = 80 };
            _hotkeyTimer.Tick += (_, _) => UpdateHotkeyRegistration();

            _uiReady.Set();
            Application.Run();
        }
        catch (Exception ex)
        {
            Log($"RunUiThread fatal error: {ex}");
            _uiReady.Set();
        }
    }

    private void RunOnUi(Action action)
    {
        if (_parkingForm == null || _parkingForm.IsDisposed) return;
        if (!_parkingForm.InvokeRequired)
        {
            action();
            return;
        }
        // Always BeginInvoke so cross-process SetParent/ShowWindow NEVER deadlocks caller's COM thread!
        _parkingForm.BeginInvoke(action);
    }

    public IntPtr HostHandle => _hostHandle;

    public void AttachAndPreview(IntPtr parentHwnd, RECT rect, string filePath)
    {
        long gen = Interlocked.Increment(ref _generation);
        RunOnUi(() => AttachAndPreviewCore(gen, parentHwnd, rect, filePath));
    }

    private RECT ResolveBounds(IntPtr parentHwnd, RECT rect)
    {
        if (rect.Width > 0 && rect.Height > 0) return rect;
        if (parentHwnd != IntPtr.Zero && NativeMethods.GetClientRect(parentHwnd, out RECT client) && client.Width > 0 && client.Height > 0)
        {
            return client;
        }
        return rect;
    }

    private void ApplyBoundsToHost(RECT rect)
    {
        int w = Math.Max(1, rect.Width);
        int h = Math.Max(1, rect.Height);
        NativeMethods.MoveWindow(_hostForm.Handle, rect.Left, rect.Top, w, h, true);
        if (_controller != null)
        {
            _controller.Bounds = new Rectangle(0, 0, w, h);
            _controller.IsVisible = true;
            _controller.NotifyParentWindowPositionChanged();
        }
    }

    private void AttachAndPreviewCore(long gen, IntPtr parentHwnd, RECT rect, string filePath)
    {
        try
        {
            if (gen != Volatile.Read(ref _generation)) return;

            _currentParentHwnd = parentHwnd;
            _currentExplorerRootHwnd = NativeMethods.GetAncestor(parentHwnd, NativeMethods.GA_ROOT);
            _currentRect = ResolveBounds(parentHwnd, rect);
            _isAttached = true;

            // Convert _hostForm to WS_CHILD and parent it into Explorer's preview container
            nint style = NativeMethods.GetWindowLongPtr(_hostForm.Handle, NativeMethods.GWL_STYLE);
            style = (style & ~NativeMethods.WS_POPUP) | NativeMethods.WS_CHILD | NativeMethods.WS_VISIBLE | NativeMethods.WS_CLIPSIBLINGS | NativeMethods.WS_CLIPCHILDREN;
            NativeMethods.SetWindowLongPtr(_hostForm.Handle, NativeMethods.GWL_STYLE, style);

            IntPtr prevParent = NativeMethods.SetParent(_hostForm.Handle, parentHwnd);
            int spErr = Marshal.GetLastWin32Error();
            IntPtr actualParent = NativeMethods.GetParent(_hostForm.Handle);

            NativeMethods.ShowWindow(_hostForm.Handle, NativeMethods.SW_SHOW);
            ApplyBoundsToHost(_currentRect);

            Log($"Attach: parent=0x{parentHwnd:X}, actualParent=0x{actualParent:X}, prevParent=0x{prevParent:X}, spErr={spErr}, inRect=({rect.Left},{rect.Top},{rect.Width}x{rect.Height}), resolved=({_currentRect.Left},{_currentRect.Top},{_currentRect.Width}x{_currentRect.Height}), vis={NativeMethods.IsWindowVisible(_hostForm.Handle)}");

            _currentMedia = null;
            try
            {
                _currentMedia = new MediaSource(filePath, gen.ToString());
                Log($"Loaded media: {_currentMedia.FileName} ({_currentMedia.FileSize} bytes, mem={_currentMedia.MemoryData != null})");
            }
            catch (Exception ex)
            {
                Log($"Failed to open media '{filePath}': {ex.Message}");
            }

            _hotkeyTimer.Start();
            UpdateHotkeyRegistration();

            if (!_initStarted)
            {
                _initStarted = true;
                _ = InitializeWebViewAsync();
            }
            else if (_webViewReady && _pageReady)
            {
                SendCurrentMediaToPage();
            }
        }
        catch (Exception ex)
        {
            Log($"AttachAndPreviewCore error: {ex}");
        }
    }

    public void UpdateRect(RECT rect)
    {
        RunOnUi(() =>
        {
            _currentRect = ResolveBounds(_currentParentHwnd, rect);
            if (!_isAttached) return;
            Log($"SetRect: ({_currentRect.Left},{_currentRect.Top},{_currentRect.Width}x{_currentRect.Height})");
            ApplyBoundsToHost(_currentRect);
        });
    }

    public void FocusHost()
    {
        RunOnUi(() =>
        {
            if (!_isAttached) return;
            try
            {
                _controller?.MoveFocus(CoreWebView2MoveFocusReason.Programmatic);
            }
            catch
            {
                NativeMethods.SetFocus(_hostForm.Handle);
            }
        });
    }

    public void Detach()
    {
        Interlocked.Increment(ref _generation);
        RunOnUi(DetachCore);
    }

    private void DetachCore()
    {
        try
        {
            _isAttached = false;
            _currentParentHwnd = IntPtr.Zero;
            _currentExplorerRootHwnd = IntPtr.Zero;

            _hotkeyTimer.Stop();
            UnregisterHotkey();

            if (_webViewReady && _pageReady && _coreWebView != null)
            {
                PostMessage(new { type = "unload" });
            }

            _currentMedia = null;

            if (_controller != null)
            {
                _controller.IsVisible = false;
            }
            NativeMethods.ShowWindow(_hostForm.Handle, NativeMethods.SW_HIDE);
            NativeMethods.SetParent(_hostForm.Handle, _parkingForm.Handle);
            NativeMethods.MoveWindow(_hostForm.Handle, 0, 0, 1, 1, false);
        }
        catch (Exception ex)
        {
            Log($"DetachCore error: {ex}");
        }
    }

    private async Task InitializeWebViewAsync()
    {
        try
        {
            if (!_loaderDllPathConfigured)
            {
                _loaderDllPathConfigured = true;
                string? asmDir = Path.GetDirectoryName(typeof(PreviewHostManager).Assembly.Location);
                if (!string.IsNullOrEmpty(asmDir) && File.Exists(Path.Combine(asmDir, "WebView2Loader.dll")))
                {
                    try { CoreWebView2Environment.SetLoaderDllFolderPath(asmDir); } catch { }
                }
            }

            // Per-process userDataFolder avoids 0x800700AA if multiple prevhost instances exist concurrently
            string userDataFolder = Path.Combine(_dataRoot, "WebView2", $"p_{Environment.ProcessId}");
            Directory.CreateDirectory(userDataFolder);

            var options = new CoreWebView2EnvironmentOptions
            {
                AdditionalBrowserArguments = "--autoplay-policy=no-user-gesture-required"
            };

            _env = await CoreWebView2Environment.CreateAsync(null, userDataFolder, options);
            _controller = await _env.CreateCoreWebView2ControllerAsync(_hostForm.Handle);
            _coreWebView = _controller.CoreWebView2;

            _coreWebView.Settings.AreDefaultContextMenusEnabled = false;
            _coreWebView.Settings.AreDevToolsEnabled = false;
            _coreWebView.Settings.IsStatusBarEnabled = false;
            _coreWebView.Settings.IsZoomControlEnabled = false;
            _coreWebView.Settings.AreBrowserAcceleratorKeysEnabled = false;

            _coreWebView.AddWebResourceRequestedFilter(VirtualOrigin + "/*", CoreWebView2WebResourceContext.All);
            _coreWebView.WebResourceRequested += OnWebResourceRequested;
            _coreWebView.WebMessageReceived += OnWebMessageReceived;

            if (_isAttached)
            {
                _currentRect = ResolveBounds(_currentParentHwnd, _currentRect);
                ApplyBoundsToHost(_currentRect);
            }
            else
            {
                _controller.IsVisible = false;
            }

            _webViewReady = true;
            Log($"WebView2 controller ready. bounds={_controller.Bounds}, isVisible={_controller.IsVisible}");
            _coreWebView.Navigate(IndexUrl);
        }
        catch (Exception ex)
        {
            Log($"InitializeWebViewAsync failed: {ex}");
            _initStarted = false;
        }
    }

    private void OnWebResourceRequested(object? sender, CoreWebView2WebResourceRequestedEventArgs e)
    {
        try
        {
            if (_env == null) return;
            var uri = new Uri(e.Request.Uri);

            if (uri.AbsolutePath.Equals("/index.html", StringComparison.OrdinalIgnoreCase))
            {
                var bytes = Encoding.UTF8.GetBytes(_htmlContent);
                var ms = new MemoryStream(bytes);
                e.Response = _env.CreateWebResourceResponse(
                    ms,
                    200,
                    "OK",
                    "Content-Type: text/html; charset=utf-8\r\nCache-Control: no-store");
                return;
            }

            if (uri.AbsolutePath.Equals("/media", StringComparison.OrdinalIgnoreCase))
            {
                var media = _currentMedia;
                if (media == null)
                {
                    e.Response = _env.CreateWebResourceResponse(new MemoryStream(), 404, "Not Found", "Content-Type: text/plain");
                    return;
                }

                long total = media.FileSize;
                string? rangeHeader = null;
                if (e.Request.Headers.Contains("Range"))
                {
                    rangeHeader = e.Request.Headers.GetHeader("Range");
                }

                if (!string.IsNullOrEmpty(rangeHeader) && TryParseRange(rangeHeader, total, out long start, out long end))
                {
                    long requestedLen = end - start + 1;
                    if (media.MemoryData == null && requestedLen > MaxStreamChunkBytes)
                    {
                        end = start + MaxStreamChunkBytes - 1;
                        requestedLen = end - start + 1;
                    }

                    byte[] chunk = media.ReadRange(start, (int)requestedLen);
                    long actualEnd = start + chunk.Length - 1;
                    var ms = new MemoryStream(chunk, writable: false);
                    string headers =
                        $"Content-Type: {media.MimeType}\r\n" +
                        $"Accept-Ranges: bytes\r\n" +
                        $"Content-Range: bytes {start}-{actualEnd}/{total}\r\n" +
                        $"Content-Length: {chunk.Length}\r\n" +
                        $"Cache-Control: no-store";
                    e.Response = _env.CreateWebResourceResponse(ms, 206, "Partial Content", headers);
                    return;
                }

                long fullLen = media.MemoryData != null ? media.MemoryData.Length : Math.Min(total, MediaSource.MaxMemoryCacheBytes);
                byte[] fullData = media.ReadRange(0, (int)fullLen);
                var fullMs = new MemoryStream(fullData, writable: false);
                string fullHeaders =
                    $"Content-Type: {media.MimeType}\r\n" +
                    $"Accept-Ranges: bytes\r\n" +
                    $"Content-Length: {fullData.Length}\r\n" +
                    $"Cache-Control: no-store";
                e.Response = _env.CreateWebResourceResponse(fullMs, 200, "OK", fullHeaders);
                return;
            }

            e.Response = _env.CreateWebResourceResponse(new MemoryStream(), 404, "Not Found", "Content-Type: text/plain");
        }
        catch (Exception ex)
        {
            Log($"OnWebResourceRequested error: {ex.Message}");
        }
    }

    private static bool TryParseRange(string rangeHeader, long totalLength, out long start, out long end)
    {
        start = 0;
        end = totalLength - 1;
        if (totalLength <= 0) return false;
        if (!rangeHeader.StartsWith("bytes=", StringComparison.OrdinalIgnoreCase)) return false;

        string spec = rangeHeader["bytes=".Length..].Split(',')[0].Trim();
        int dash = spec.IndexOf('-');
        if (dash < 0) return false;

        string startStr = spec[..dash].Trim();
        string endStr = spec[(dash + 1)..].Trim();

        if (string.IsNullOrEmpty(startStr))
        {
            if (!long.TryParse(endStr, out long suffix) || suffix <= 0) return false;
            start = Math.Max(0, totalLength - suffix);
            end = totalLength - 1;
            return true;
        }

        if (!long.TryParse(startStr, out start) || start < 0 || start >= totalLength) return false;

        if (string.IsNullOrEmpty(endStr))
        {
            end = totalLength - 1;
        }
        else
        {
            if (!long.TryParse(endStr, out end) || end < start) return false;
            if (end >= totalLength) end = totalLength - 1;
        }
        return true;
    }

    private void OnWebMessageReceived(object? sender, CoreWebView2WebMessageReceivedEventArgs e)
    {
        try
        {
            using var doc = JsonDocument.Parse(e.WebMessageAsJson);
            if (!doc.RootElement.TryGetProperty("type", out var typeProp)) return;
            string? type = typeProp.GetString();

            if (type == "ready")
            {
                _pageReady = true;
                Log("HTML player page ready.");
                if (_isAttached)
                {
                    SendCurrentMediaToPage();
                }
            }
            else if (type == "log" && doc.RootElement.TryGetProperty("message", out var msgEl))
            {
                Log("UI: " + msgEl.GetString());
            }
            else if (type == "saveSettings")
            {
                if (doc.RootElement.TryGetProperty("settings", out var settingsEl))
                {
                    var updated = JsonSerializer.Deserialize<UserSettings>(settingsEl.GetRawText(), JsonOpts);
                    if (updated != null)
                    {
                        _settings = updated;
                        SaveSettings(_settings);
                    }
                }
            }
        }
        catch (Exception ex)
        {
            Log($"OnWebMessageReceived error: {ex.Message}");
        }
    }

    private void SendCurrentMediaToPage()
    {
        if (!_webViewReady || !_pageReady || _coreWebView == null) return;

        var media = _currentMedia;
        if (media == null)
        {
            PostMessage(new { type = "error", message = "无法读取该媒体文件" });
            return;
        }

        PostMessage(new
        {
            type = "load",
            url = $"{VirtualOrigin}/media?v={media.Token}",
            fileName = media.FileName,
            extension = media.Extension.TrimStart('.').ToUpperInvariant(),
            isVideo = media.IsVideo,
            fileSize = media.FileSize,
            canDecodeWaveform = !media.IsVideo || media.FileSize <= 25L * 1024L * 1024L,
            settings = _settings
        });
    }

    private void PostMessage(object payload)
    {
        try
        {
            string json = JsonSerializer.Serialize(payload, JsonOpts);
            _coreWebView?.PostWebMessageAsJson(json);
        }
        catch (Exception ex)
        {
            Log($"PostMessage error: {ex.Message}");
        }
    }

    private void OnSpaceHotkey()
    {
        if (!_isAttached || !_webViewReady || !_pageReady) return;
        if (!ShouldCaptureSpace()) return;
        Log("Space hotkey triggered -> togglePlay");
        PostMessage(new { type = "togglePlay" });
    }

    private void UpdateHotkeyRegistration()
    {
        bool shouldCapture = ShouldCaptureSpace();
        if (shouldCapture && !_hotkeyRegistered)
        {
            _hotkeyRegistered = NativeMethods.RegisterHotKey(_hotkeyWindow.Handle, HotkeyMessageWindow.HotkeyId, 0, NativeMethods.VK_SPACE);
            if (!_hotkeyRegistered) Log($"RegisterHotKey(SPACE) failed, err={Marshal.GetLastWin32Error()}");
        }
        else if (!shouldCapture && _hotkeyRegistered)
        {
            UnregisterHotkey();
        }
    }

    private void UnregisterHotkey()
    {
        if (_hotkeyRegistered)
        {
            NativeMethods.UnregisterHotKey(_hotkeyWindow.Handle, HotkeyMessageWindow.HotkeyId);
            _hotkeyRegistered = false;
        }
    }

    private bool ShouldCaptureSpace()
    {
        if (!_isAttached || _currentExplorerRootHwnd == IntPtr.Zero) return false;

        IntPtr fg = NativeMethods.GetForegroundWindow();
        if (fg != _currentExplorerRootHwnd) return false;

        uint tid = NativeMethods.GetWindowThreadProcessId(fg, out _);
        if (tid == 0) return false;

        var info = new GUITHREADINFO { cbSize = Marshal.SizeOf<GUITHREADINFO>() };
        if (NativeMethods.GetGUIThreadInfo(tid, ref info))
        {
            if (info.hwndCaret != IntPtr.Zero) return false;

            IntPtr focus = info.hwndFocus;
            if (focus != IntPtr.Zero)
            {
                if (focus == _hostForm.Handle || NativeMethods.IsChild(_hostForm.Handle, focus))
                {
                    return false;
                }

                string cls = NativeMethods.GetWindowClassName(focus);
                if (cls.Contains("Edit", StringComparison.OrdinalIgnoreCase) ||
                    cls.Contains("ComboBox", StringComparison.OrdinalIgnoreCase) ||
                    cls.Contains("Search", StringComparison.OrdinalIgnoreCase) ||
                    cls.Contains("InputSite", StringComparison.OrdinalIgnoreCase) ||
                    cls.Contains("Xaml", StringComparison.OrdinalIgnoreCase))
                {
                    return false;
                }
            }
        }

        IntPtr localFocus = NativeMethods.GetFocus();
        if (localFocus != IntPtr.Zero && (localFocus == _hostForm.Handle || NativeMethods.IsChild(_hostForm.Handle, localFocus)))
        {
            return false;
        }

        return true;
    }

    private UserSettings LoadSettings()
    {
        try
        {
            if (File.Exists(_settingsPath))
            {
                string json = File.ReadAllText(_settingsPath, Encoding.UTF8);
                return JsonSerializer.Deserialize<UserSettings>(json, JsonOpts) ?? new UserSettings();
            }
        }
        catch { }
        return new UserSettings();
    }

    private void SaveSettings(UserSettings settings)
    {
        try
        {
            string json = JsonSerializer.Serialize(settings, JsonOpts);
            File.WriteAllText(_settingsPath, json, Encoding.UTF8);
        }
        catch { }
    }

    private static string LoadEmbeddedHtml()
    {
        var asm = Assembly.GetExecutingAssembly();
        using var stream = asm.GetManifestResourceStream("MediaPreviewPane.Assets.player.html")
            ?? throw new InvalidOperationException("Embedded player.html resource not found.");
        using var reader = new StreamReader(stream, Encoding.UTF8);
        return reader.ReadToEnd();
    }

    public void Log(string message)
    {
        try
        {
            var fi = new FileInfo(_logPath);
            if (fi.Exists && fi.Length > 200_000)
            {
                File.WriteAllText(_logPath, string.Empty);
            }
            File.AppendAllText(_logPath, $"[{DateTime.Now:HH:mm:ss.fff}] {message}{Environment.NewLine}", Encoding.UTF8);
        }
        catch { }
    }
}