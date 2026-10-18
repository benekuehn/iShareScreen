//! The C bridge owns Wayland proxies and their bounded event/feedback lists.
//! Destruction requires the render worker and main-thread pump to have stopped.
use std::ffi::{c_int, c_void};

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct PresentationEvent {
    frame_id: u64,
    submit_ns: u64,
    decode_submit_ns: u64,
    gpu_ready_ns: u64,
    presented_ns: u64,
    refresh_ns: u64,
    sequence: u64,
    flags: u64,
    discarded: u64,
}
unsafe extern "C" {
    fn bridge_presentation_new(display: *mut c_void, surface: *mut c_void) -> *mut c_void;
    fn bridge_presentation_free(ptr: *mut c_void);
    fn bridge_presentation_request(
        ptr: *mut c_void,
        id: u64,
        decode_ns: u64,
        ready_ns: u64,
    ) -> c_int;
    fn bridge_presentation_pump(ptr: *mut c_void) -> c_int;
    fn bridge_presentation_read(
        ptr: *mut c_void,
        events: *mut PresentationEvent,
        capacity: usize,
    ) -> usize;
    fn bridge_presentation_clock(ptr: *mut c_void) -> c_int;
    fn bridge_presentation_dropped(ptr: *mut c_void) -> u64;
    fn bridge_presentation_frame_pending(ptr: *mut c_void) -> usize;
}
struct Presentation(*mut c_void);
impl Drop for Presentation {
    fn drop(&mut self) {
        unsafe { bridge_presentation_free(self.0) };
    }
}
unsafe fn inner(ptr: *mut c_void) -> *mut c_void {
    if ptr.is_null() {
        std::ptr::null_mut()
    } else {
        unsafe { (*ptr.cast::<Presentation>()).0 }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_presentation_new(
    display: *mut c_void,
    surface: *mut c_void,
) -> *mut c_void {
    let ptr = unsafe { bridge_presentation_new(display, surface) };
    if ptr.is_null() {
        ptr
    } else {
        Box::into_raw(Box::new(Presentation(ptr))).cast()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_presentation_free(ptr: *mut c_void) {
    if !ptr.is_null() {
        drop(unsafe { Box::from_raw(ptr.cast::<Presentation>()) });
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_presentation_request(
    ptr: *mut c_void,
    id: u64,
    decode_ns: u64,
    ready_ns: u64,
) -> c_int {
    unsafe { bridge_presentation_request(inner(ptr), id, decode_ns, ready_ns) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_presentation_pump(ptr: *mut c_void) -> c_int {
    unsafe { bridge_presentation_pump(inner(ptr)) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_presentation_read(
    ptr: *mut c_void,
    events: *mut PresentationEvent,
    capacity: usize,
) -> usize {
    unsafe { bridge_presentation_read(inner(ptr), events, capacity) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_presentation_clock(ptr: *mut c_void) -> c_int {
    unsafe { bridge_presentation_clock(inner(ptr)) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_presentation_dropped(ptr: *mut c_void) -> u64 {
    unsafe { bridge_presentation_dropped(inner(ptr)) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_presentation_frame_pending(ptr: *mut c_void) -> usize {
    unsafe { bridge_presentation_frame_pending(inner(ptr)) }
}
