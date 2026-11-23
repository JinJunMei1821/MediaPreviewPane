use std::cell::RefCell;
use std::ffi::c_void;
use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::UNIX_EPOCH;

use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_FAIL, E_NOINTERFACE, E_NOTIMPL, E_POINTER,
    HWND, RECT, S_FALSE, S_OK,
};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl, IStream, STATFLAG_DEFAULT, STATSTG};
use windows::Win32::System::Ole::{IObjectWithSite, IObjectWithSite_Impl, IOleWindow, IOleWindow_Impl};
use windows::Win32::UI::Input::KeyboardAndMouse::GetFocus;
use windows::Win32::UI::Shell::PropertiesSystem::{
    IInitializeWithFile, IInitializeWithFile_Impl, IInitializeWithStream, IInitializeWithStream_Impl,
};
use windows::Win32::UI::Shell::{IPreviewHandler, IPreviewHandler_Impl, IPreviewHandlerFrame};
use windows::Win32::UI::WindowsAndMessaging::MSG;
use windows::core::{BOOL, Error, GUID, HRESULT, IUnknown, Interface, PCWSTR, Ref, implement};

use crate::settings::log_msg;
use crate::ui::{MediaInput, MediaKind, PreviewWindow, classify_extension, file_name_from_path};

/// {9F4E2B61-7A3D-4E8C-9B12-5D8A1E4C7F30}
pub const CLSID_MEDIA_PREVIEW_HANDLER: GUID =
    GUID::from_u128(0x9F4E2B61_7A3D_4E8C_9B12_5D8A1E4C7F30);

const MAX_AUDIO_MEM_BYTES: u64 = 128 * 1024 * 1024;
const MAX_VIDEO_MEM_BYTES: u64 = 48 * 1024 * 1024;

static DLL_REF_COUNT: AtomicUsize = AtomicUsize::new(0);

pub fn can_unload_now() -> HRESULT {
    if DLL_REF_COUNT.load(Ordering::Acquire) == 0 {
        S_OK
    } else {
        S_FALSE
    }
}

pub unsafe fn get_class_object(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    if ppv.is_null() {
        return E_POINTER;
    }
    unsafe { *ppv = std::ptr::null_mut(); }
    if rclsid.is_null() || riid.is_null() {
        return E_POINTER;
    }
    if unsafe { *rclsid } != CLSID_MEDIA_PREVIEW_HANDLER {
        return CLASS_E_CLASSNOTAVAILABLE;
    }

    let factory: IClassFactory = PreviewClassFactory::new().into();
    unsafe { factory.query(riid, ppv) }
}

#[implement(IClassFactory)]
struct PreviewClassFactory;

impl PreviewClassFactory {
    fn new() -> Self {
        DLL_REF_COUNT.fetch_add(1, Ordering::Relaxed);
        Self
    }
}

impl Drop for PreviewClassFactory {
    fn drop(&mut self) {
        DLL_REF_COUNT.fetch_sub(1, Ordering::Release);
    }
}

impl IClassFactory_Impl for PreviewClassFactory_Impl {
    fn CreateInstance(
        &self,
        punkouter: Ref<'_, IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut c_void,
    ) -> windows::core::Result<()> {
        if ppvobject.is_null() || riid.is_null() {
            return Err(Error::from(E_POINTER));
        }
        unsafe { *ppvobject = std::ptr::null_mut(); }
        if punkouter.is_some() {
            return Err(Error::from(CLASS_E_NOAGGREGATION));
        }
        let handler: IPreviewHandler = MediaPreviewHandler::new().into();
        let hr = unsafe { handler.query(riid, ppvobject) };
        hr.ok()
    }

    fn LockServer(&self, flock: BOOL) -> windows::core::Result<()> {
        if flock.as_bool() {
            DLL_REF_COUNT.fetch_add(1, Ordering::Relaxed);
        } else {
            DLL_REF_COUNT.fetch_sub(1, Ordering::Release);
        }
        Ok(())
    }
}

struct HandlerInner {
    parent_hwnd: HWND,
    rect: RECT,
    site: Option<IUnknown>,
    pending_input: Option<MediaInput>,
    window: Option<Box<PreviewWindow>>,
}

#[implement(
    IPreviewHandler,
    IInitializeWithFile,
    IInitializeWithStream,
    IOleWindow,
    IObjectWithSite
)]
struct MediaPreviewHandler {
    inner: RefCell<HandlerInner>,
}

impl MediaPreviewHandler {
    fn new() -> Self {
        DLL_REF_COUNT.fetch_add(1, Ordering::Relaxed);
        Self {
            inner: RefCell::new(HandlerInner {
                parent_hwnd: HWND::default(),
                rect: RECT::default(),
                site: None,
                pending_input: None,
                window: None,
            }),
        }
    }
}

impl Drop for MediaPreviewHandler {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.inner.try_borrow_mut() {
            if let Some(mut w) = inner.window.take() {
                w.destroy_window();
            }
            inner.site = None;
            inner.pending_input = None;
        }
        DLL_REF_COUNT.fetch_sub(1, Ordering::Release);
    }
}

impl IInitializeWithFile_Impl for MediaPreviewHandler_Impl {
    fn Initialize(&self, pszfilepath: &PCWSTR, _grfmode: u32) -> windows::core::Result<()> {
        let path = unsafe { pszfilepath.to_string() }.map_err(|_| Error::from(E_FAIL))?;
        if path.is_empty() {
            return Err(Error::from(E_FAIL));
        }

        let (file_name, ext) = file_name_from_path(&path);
        let kind = classify_extension(&ext);

        let meta = fs::metadata(&path).map_err(|e| {
            log_msg(&format!("metadata failed for {path}: {e}"));
            Error::from(E_FAIL)
        })?;
        let file_size = meta.len();
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);

        let max_mem = if kind == MediaKind::Audio {
            MAX_AUDIO_MEM_BYTES
        } else {
            MAX_VIDEO_MEM_BYTES
        };

        let mem_data = if file_size <= max_mem {
            fs::read(&path).ok().map(Arc::new)
        } else {
            None
        };

        let mut inner = self.inner.borrow_mut();
        inner.pending_input = Some(MediaInput {
            file_path: Some(path),
            file_name,
            ext,
            file_size,
            mtime_ns,
            mem_data,
            kind,
        });
        Ok(())
    }
}

impl IInitializeWithStream_Impl for MediaPreviewHandler_Impl {
    fn Initialize(&self, pstream: Ref<'_, IStream>, _grfmode: u32) -> windows::core::Result<()> {
        let Some(stream) = pstream.as_ref() else {
            return Err(Error::from(E_POINTER));
        };

        let mut stat = STATSTG::default();
        let mut name = "media".to_string();
        let mut ext = String::new();
        let mut size_hint = 0usize;

        if unsafe { stream.Stat(&mut stat, STATFLAG_DEFAULT) }.is_ok() {
            size_hint = (stat.cbSize as usize).min(MAX_AUDIO_MEM_BYTES as usize);
            if !stat.pwcsName.is_null() {
                if let Ok(s) = unsafe { stat.pwcsName.to_string() } {
                    let (n, e) = file_name_from_path(&s);
                    name = n;
                    ext = e;
                }
                unsafe {
                    windows::Win32::System::Com::CoTaskMemFree(Some(stat.pwcsName.0.cast()));
                }
            }
        }

        let mut bytes = Vec::with_capacity(size_hint);
        let mut chunk = [0u8; 65536];
        loop {
            let mut read = 0u32;
            let hr = unsafe { stream.Read(chunk.as_mut_ptr().cast(), chunk.len() as u32, Some(&mut read)) };
            if read > 0 {
                bytes.extend_from_slice(&chunk[..read as usize]);
                if bytes.len() as u64 >= MAX_AUDIO_MEM_BYTES {
                    break;
                }
            }
            if hr.is_err() || read == 0 {
                break;
            }
        }

        let file_size = bytes.len() as u64;
        let kind = classify_extension(&ext);

        let mut inner = self.inner.borrow_mut();
        inner.pending_input = Some(MediaInput {
            file_path: None,
            file_name: name,
            ext,
            file_size,
            mtime_ns: 0,
            mem_data: Some(Arc::new(bytes)),
            kind,
        });
        Ok(())
    }
}

impl IPreviewHandler_Impl for MediaPreviewHandler_Impl {
    fn SetWindow(&self, hwnd: HWND, prc: *const RECT) -> windows::core::Result<()> {
        let mut inner = self.inner.borrow_mut();
        inner.parent_hwnd = hwnd;
        if !prc.is_null() {
            inner.rect = unsafe { *prc };
        }
        let rc = inner.rect;
        if let Some(w) = &mut inner.window {
            w.resize(rc);
        }
        Ok(())
    }

    fn SetRect(&self, prc: *const RECT) -> windows::core::Result<()> {
        if prc.is_null() {
            return Err(Error::from(E_POINTER));
        }
        let mut inner = self.inner.borrow_mut();
        let rc = unsafe { *prc };
        inner.rect = rc;
        if let Some(w) = &mut inner.window {
            w.resize(rc);
        }
        Ok(())
    }

    fn DoPreview(&self) -> windows::core::Result<()> {
        let mut inner = self.inner.borrow_mut();
        if let Some(mut old) = inner.window.take() {
            old.destroy_window();
        }
        if inner.parent_hwnd.0.is_null() {
            return Err(Error::from(E_FAIL));
        }
        let Some(input) = inner.pending_input.take() else {
            return Err(Error::from(E_FAIL));
        };

        let parent = inner.parent_hwnd;
        let rc = inner.rect;
        match PreviewWindow::create(parent, rc, input) {
            Ok(win) => {
                inner.window = Some(win);
                Ok(())
            }
            Err(e) => {
                log_msg(&format!("DoPreview failed: {e}"));
                Err(Error::from(E_FAIL))
            }
        }
    }

    fn Unload(&self) -> windows::core::Result<()> {
        let mut inner = self.inner.borrow_mut();
        if let Some(mut w) = inner.window.take() {
            w.destroy_window();
        }
        inner.pending_input = None;
        Ok(())
    }

    fn SetFocus(&self) -> windows::core::Result<()> {
        let inner = self.inner.borrow();
        if let Some(w) = &inner.window {
            w.focus();
        }
        Ok(())
    }

    fn QueryFocus(&self) -> windows::core::Result<HWND> {
        Ok(unsafe { GetFocus() })
    }

    fn TranslateAccelerator(&self, pmsg: *const MSG) -> windows::core::Result<()> {
        if pmsg.is_null() {
            return Err(Error::from(E_POINTER));
        }
        let inner = self.inner.borrow();
        if let Some(site) = &inner.site {
            if let Ok(frame) = site.cast::<IPreviewHandlerFrame>() {
                return unsafe { frame.TranslateAccelerator(pmsg) };
            }
        }
        Err(Error::from(S_FALSE))
    }
}

impl IOleWindow_Impl for MediaPreviewHandler_Impl {
    fn GetWindow(&self) -> windows::core::Result<HWND> {
        let inner = self.inner.borrow();
        if let Some(w) = &inner.window {
            Ok(w.hwnd())
        } else {
            Ok(inner.parent_hwnd)
        }
    }

    fn ContextSensitiveHelp(&self, _fentermode: BOOL) -> windows::core::Result<()> {
        Err(Error::from(E_NOTIMPL))
    }
}

impl IObjectWithSite_Impl for MediaPreviewHandler_Impl {
    fn SetSite(&self, punksite: Ref<'_, IUnknown>) -> windows::core::Result<()> {
        let mut inner = self.inner.borrow_mut();
        inner.site = punksite.cloned();
        Ok(())
    }

    fn GetSite(&self, riid: *const GUID, ppvsite: *mut *mut c_void) -> windows::core::Result<()> {
        if ppvsite.is_null() || riid.is_null() {
            return Err(Error::from(E_POINTER));
        }
        unsafe { *ppvsite = std::ptr::null_mut(); }
        let inner = self.inner.borrow();
        if let Some(site) = &inner.site {
            unsafe { site.query(riid, ppvsite) }.ok()
        } else {
            Err(Error::from(E_NOINTERFACE))
        }
    }
}