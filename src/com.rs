use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_FAIL, E_NOINTERFACE, E_NOTIMPL, E_POINTER,
    HWND, RECT, S_FALSE, S_OK,
};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::System::Ole::{IObjectWithSite, IObjectWithSite_Impl, IOleWindow, IOleWindow_Impl};
use windows::Win32::UI::Input::KeyboardAndMouse::GetFocus;
use windows::Win32::UI::Shell::PropertiesSystem::{IInitializeWithFile, IInitializeWithFile_Impl};
use windows::Win32::UI::Shell::{IPreviewHandler, IPreviewHandler_Impl, IPreviewHandlerFrame};
use windows::Win32::UI::WindowsAndMessaging::MSG;
use windows::core::{BOOL, Error, GUID, HRESULT, IUnknown, Interface, PCWSTR, Ref, implement};

use crate::ui::PreviewUiHost;

/// {9F4E2B61-7A3D-4E8C-9B12-5D8A1E4C7F30}
pub const CLSID_MEDIA_PREVIEW_HANDLER: GUID =
    GUID::from_u128(0x9F4E2B61_7A3D_4E8C_9B12_5D8A1E4C7F30);

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
    file_path: Option<String>,
    active_gen: Option<u64>,
}

#[implement(
    IPreviewHandler,
    IInitializeWithFile,
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
                file_path: None,
                active_gen: None,
            }),
        }
    }
}

impl Drop for MediaPreviewHandler {
    fn drop(&mut self) {
        let epoch = if let Ok(mut inner) = self.inner.try_borrow_mut() {
            inner.site = None;
            inner.file_path = None;
            inner.active_gen.take()
        } else {
            None
        };
        if let Some(g) = epoch {
            PreviewUiHost::get().detach(g);
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
        if let Ok(mut inner) = self.inner.try_borrow_mut() {
            inner.file_path = Some(path);
        }
        Ok(())
    }
}

impl IPreviewHandler_Impl for MediaPreviewHandler_Impl {
    fn SetWindow(&self, hwnd: HWND, prc: *const RECT) -> windows::core::Result<()> {
        let rc = if !prc.is_null() {
            Some(unsafe { *prc })
        } else {
            None
        };
        let active_update = if let Ok(mut inner) = self.inner.try_borrow_mut() {
            inner.parent_hwnd = hwnd;
            if let Some(r) = rc {
                inner.rect = r;
            }
            inner.active_gen.map(|g| (g, hwnd, inner.rect))
        } else {
            None
        };
        if let Some((g, parent, r)) = active_update {
            PreviewUiHost::get().reparent_or_resize(g, parent, r);
        }
        Ok(())
    }

    fn SetRect(&self, prc: *const RECT) -> windows::core::Result<()> {
        if prc.is_null() {
            return Err(Error::from(E_POINTER));
        }
        let rc = unsafe { *prc };
        let active_gen = if let Ok(mut inner) = self.inner.try_borrow_mut() {
            inner.rect = rc;
            inner.active_gen
        } else {
            None
        };
        if let Some(g) = active_gen {
            PreviewUiHost::get().resize(g, rc);
        }
        Ok(())
    }

    fn DoPreview(&self) -> windows::core::Result<()> {
        let (parent, rc, path) = {
            let inner = self.inner.borrow();
            if inner.parent_hwnd.0.is_null() {
                return Err(Error::from(E_FAIL));
            }
            let Some(path) = inner.file_path.clone() else {
                return Err(Error::from(E_FAIL));
            };
            (inner.parent_hwnd, inner.rect, path)
        };

        let epoch = PreviewUiHost::get().attach_and_preview(parent, rc, path);
        if let Ok(mut inner) = self.inner.try_borrow_mut() {
            inner.active_gen = Some(epoch);
        }
        Ok(())
    }

    fn Unload(&self) -> windows::core::Result<()> {
        let epoch = if let Ok(mut inner) = self.inner.try_borrow_mut() {
            inner.file_path = None;
            inner.parent_hwnd = HWND::default();
            inner.active_gen.take()
        } else {
            None
        };
        if let Some(g) = epoch {
            PreviewUiHost::get().detach(g);
        }
        Ok(())
    }

    fn SetFocus(&self) -> windows::core::Result<()> {
        let epoch = self.inner.borrow().active_gen;
        if let Some(g) = epoch {
            PreviewUiHost::get().focus(g);
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
        let site = self.inner.borrow().site.clone();
        if let Some(s) = site {
            if let Ok(frame) = s.cast::<IPreviewHandlerFrame>() {
                return unsafe { frame.TranslateAccelerator(pmsg) };
            }
        }
        Err(Error::from(S_FALSE))
    }
}

impl IOleWindow_Impl for MediaPreviewHandler_Impl {
    fn GetWindow(&self) -> windows::core::Result<HWND> {
        let (is_active, parent) = {
            let inner = self.inner.borrow();
            (inner.active_gen.is_some(), inner.parent_hwnd)
        };
        if is_active {
            let hwnd = PreviewUiHost::get().preview_hwnd();
            if !hwnd.0.is_null() {
                return Ok(hwnd);
            }
        }
        Ok(parent)
    }

    fn ContextSensitiveHelp(&self, _fentermode: BOOL) -> windows::core::Result<()> {
        Err(Error::from(E_NOTIMPL))
    }
}

impl IObjectWithSite_Impl for MediaPreviewHandler_Impl {
    fn SetSite(&self, punksite: Ref<'_, IUnknown>) -> windows::core::Result<()> {
        if let Ok(mut inner) = self.inner.try_borrow_mut() {
            inner.site = punksite.cloned();
        }
        Ok(())
    }

    fn GetSite(&self, riid: *const GUID, ppvsite: *mut *mut c_void) -> windows::core::Result<()> {
        if ppvsite.is_null() || riid.is_null() {
            return Err(Error::from(E_POINTER));
        }
        unsafe { *ppvsite = std::ptr::null_mut(); }
        let site = self.inner.borrow().site.clone();
        if let Some(s) = site {
            unsafe { s.query(riid, ppvsite) }.ok()
        } else {
            Err(Error::from(E_NOINTERFACE))
        }
    }
}