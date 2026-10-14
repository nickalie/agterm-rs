//! The class and instance structs the WPE host subclasses (a `WPEDisplay` and a `WPEView` of its own, the web view it
//! drives) match the types registered at run time, and carry the virtual functions that subclass fills with the
//! signatures it implements. The generated ABI tests check the same structs against the C headers; this checks them
//! against the linked library.

use std::ffi::c_uint;
use std::mem::{MaybeUninit, size_of};

use agterm_wpe_sys::{WebKitWebView, WebKitWebViewClass, webkit_web_view_get_type};
use glib_sys::{GError, GType, gboolean};
use gobject_sys::{GTypeQuery, g_type_query};
use wpe_platform_sys::{
    WPEBuffer, WPEBufferFormats, WPEDRMDevice, WPEDisplay, WPEDisplayClass, WPERectangle, WPEView, WPEViewClass,
    wpe_display_get_type, wpe_view_get_type,
};

/// The class and instance sizes GObject registered for `gtype`.
fn registered_sizes(gtype: GType) -> (usize, usize) {
    let mut query = MaybeUninit::<GTypeQuery>::zeroed();
    // SAFETY: `gtype` is a registered type and `query` is writable storage for one GTypeQuery.
    unsafe { g_type_query(gtype, query.as_mut_ptr()) };
    // SAFETY: GTypeQuery is plain data, zeroed above and filled in by g_type_query.
    let query = unsafe { query.assume_init() };
    assert_eq!(query.type_, gtype, "g_type_query did not recognise the type");
    (query.class_size as usize, query.instance_size as usize)
}

#[test]
fn display_view_and_web_view_structs_match_the_registered_types() {
    // SAFETY: the get_type functions register the type on first use and take no arguments.
    let (display, view, web_view) =
        unsafe { (wpe_display_get_type(), wpe_view_get_type(), webkit_web_view_get_type()) };
    assert_eq!(
        registered_sizes(display),
        (size_of::<WPEDisplayClass>(), size_of::<WPEDisplay>())
    );
    assert_eq!(
        registered_sizes(view),
        (size_of::<WPEViewClass>(), size_of::<WPEView>())
    );
    assert_eq!(
        registered_sizes(web_view),
        (size_of::<WebKitWebViewClass>(), size_of::<WebKitWebView>())
    );
}

unsafe extern "C" fn connect(_: *mut WPEDisplay, _: *mut *mut GError) -> gboolean {
    1
}
unsafe extern "C" fn create_view(_: *mut WPEDisplay) -> *mut WPEView {
    std::ptr::null_mut()
}
unsafe extern "C" fn get_drm_device(_: *mut WPEDisplay) -> *mut WPEDRMDevice {
    std::ptr::null_mut()
}
unsafe extern "C" fn get_preferred_buffer_formats(_: *mut WPEDisplay) -> *mut WPEBufferFormats {
    std::ptr::null_mut()
}
unsafe extern "C" fn render_buffer(
    _: *mut WPEView,
    _: *mut WPEBuffer,
    _: *const WPERectangle,
    _: c_uint,
    _: *mut *mut GError,
) -> gboolean {
    1
}
unsafe extern "C" fn buffers_changed(_: *mut WPEView, _: *mut *mut WPEBuffer, _: c_uint) {}

#[test]
fn display_and_view_classes_take_the_virtual_functions_a_subclass_fills() {
    // SAFETY: the class structs are plain C data (GObject class structs with nullable function pointers).
    let mut display = unsafe { MaybeUninit::<WPEDisplayClass>::zeroed().assume_init() };
    display.connect = Some(connect);
    display.create_view = Some(create_view);
    display.get_drm_device = Some(get_drm_device);
    display.get_preferred_buffer_formats = Some(get_preferred_buffer_formats);
    // SAFETY: as above.
    let mut view = unsafe { MaybeUninit::<WPEViewClass>::zeroed().assume_init() };
    view.render_buffer = Some(render_buffer);
    view.buffers_changed = Some(buffers_changed);
    assert!(display.connect.is_some() && display.create_view.is_some());
    assert!(display.get_drm_device.is_some() && display.get_preferred_buffer_formats.is_some());
    assert!(view.render_buffer.is_some() && view.buffers_changed.is_some());
}
