using System;
using System.Runtime.InteropServices;

namespace MediaPreviewPane;

[ComVisible(true)]
[Guid("9F4E2B61-7A3D-4E8C-9B12-5D8A1E4C7F30")]
[ClassInterface(ClassInterfaceType.None)]
[ProgId("MediaPreviewPane.Handler")]
public sealed class MediaPreviewHandler :
    IPreviewHandler,
    IInitializeWithFile,
    IObjectWithSite,
    IOleWindow
{
    private static MediaPreviewHandler? _activeOwner;

    private string? _filePath;
    private IntPtr _parentHwnd;
    private RECT _bounds;
    private object? _site;
    private IPreviewHandlerFrame? _frame;

    public void Initialize(string pszFilePath, uint grfMode)
    {
        _filePath = pszFilePath;
    }

    public void SetWindow(IntPtr hwnd, ref RECT rect)
    {
        _parentHwnd = hwnd;
        _bounds = rect;

        if (_activeOwner == this)
        {
            PreviewHostManager.Instance.UpdateRect(_bounds);
        }
    }

    public void SetRect(ref RECT rect)
    {
        _bounds = rect;
        if (_activeOwner == this)
        {
            PreviewHostManager.Instance.UpdateRect(_bounds);
        }
    }

    public void DoPreview()
    {
        if (string.IsNullOrEmpty(_filePath) || _parentHwnd == IntPtr.Zero) return;

        _activeOwner = this;
        PreviewHostManager.Instance.AttachAndPreview(_parentHwnd, _bounds, _filePath);
    }

    public void Unload()
    {
        if (_activeOwner == this)
        {
            _activeOwner = null;
            PreviewHostManager.Instance.Detach();
        }
        _filePath = null;
        _parentHwnd = IntPtr.Zero;
    }

    public void SetFocus()
    {
        if (_activeOwner == this)
        {
            PreviewHostManager.Instance.FocusHost();
        }
    }

    public void QueryFocus(out IntPtr phwnd)
    {
        phwnd = NativeMethods.GetFocus();
    }

    public uint TranslateAccelerator(ref MSG pmsg)
    {
        if (_frame != null)
        {
            return _frame.TranslateAccelerator(ref pmsg);
        }
        return NativeMethods.S_FALSE;
    }

    public void SetSite(object? pUnkSite)
    {
        _site = pUnkSite;
        _frame = pUnkSite as IPreviewHandlerFrame;
    }

    public void GetSite(ref Guid riid, out object? ppvSite)
    {
        ppvSite = _site;
    }

    public void GetWindow(out IntPtr phwnd)
    {
        phwnd = _activeOwner == this ? PreviewHostManager.Instance.HostHandle : _parentHwnd;
    }

    public void ContextSensitiveHelp(bool fEnterMode)
    {
    }
}