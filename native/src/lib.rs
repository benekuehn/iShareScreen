//! Narrow ABI around FFmpeg/VAAPI and EGL. Decoded surfaces remain owned until
//! composition finishes. The C bridge hides FFmpeg's versioned structure ABI;
//! callers never manufacture AVFrame pointers or access mapped pixel arrays.
use std::ffi::{c_char, c_int, c_void};
use std::ptr;
pub mod presentation;
mod udp;

#[repr(C)]
#[derive(Default, Debug, Copy, Clone)]
pub struct FrameInfo {
    pub pts: i64,
    pub width: i32,
    pub height: i32,
    pub error: i32,
    pub full_range: i32,
    pub matrix: i32,
    pub chroma444: i32,
}

unsafe extern "C" {
    fn bridge_decoder_new(
        codec: c_int,
        extra: *const u8,
        len: usize,
        device: *const c_char,
    ) -> *mut c_void;
    fn bridge_decoder_free(ptr: *mut c_void);
    fn bridge_decoder_flush(ptr: *mut c_void);
    fn bridge_decoder_drain(ptr: *mut c_void) -> c_int;
    fn bridge_decoder_send(ptr: *mut c_void, data: *const u8, len: usize, pts: i64) -> c_int;
    fn bridge_decoder_receive(ptr: *mut c_void) -> *mut c_void;
    fn bridge_frame_free(ptr: *mut c_void);
    fn bridge_frame_ready(ptr: *mut c_void) -> c_int;
    fn bridge_frame_wait(ptr: *mut c_void) -> c_int;
    fn bridge_frame_info(ptr: *mut c_void, info: *mut FrameInfo);
    fn bridge_renderer_new(w: c_int, h: c_int) -> *mut c_void;
    fn bridge_renderer_free(ptr: *mut c_void);
    fn bridge_renderer_upload(
        renderer: *mut c_void,
        frame: *mut c_void,
        y: c_int,
        rows: c_int,
    ) -> c_int;
    fn bridge_renderer_draw(renderer: *mut c_void, w: c_int, h: c_int) -> c_int;
    fn bridge_error() -> *const c_char;
}

struct Decoder {
    inner: *mut c_void,
    generation: u64,
}
impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { bridge_decoder_free(self.inner) };
    }
}
struct Frame {
    inner: *mut c_void,
    generation: u64,
}
impl Drop for Frame {
    fn drop(&mut self) {
        unsafe { bridge_frame_free(self.inner) };
    }
}
struct Renderer {
    inner: *mut c_void,
    generation: u64,
    width: i32,
    height: i32,
}
impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe { bridge_renderer_free(self.inner) };
    }
}

// All handles are opaque allocations owned by the corresponding Python/Rust
// wrapper. Each function requires a live handle from its matching constructor;
// decoder calls are serialized, renderer calls stay on its GL context thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_error() -> *const c_char {
    unsafe { bridge_error() }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_decoder_new(
    codec: c_int,
    extra: *const u8,
    len: usize,
    device: *const c_char,
    generation: u64,
) -> *mut c_void {
    let inner = unsafe { bridge_decoder_new(codec, extra, len, device) };
    if inner.is_null() {
        return ptr::null_mut();
    }
    Box::into_raw(Box::new(Decoder { inner, generation })).cast()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_decoder_free(ptr: *mut c_void) {
    if !ptr.is_null() {
        drop(unsafe { Box::from_raw(ptr.cast::<Decoder>()) });
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_decoder_drain(ptr: *mut c_void) -> c_int {
    if ptr.is_null() {
        return -1;
    }
    unsafe { bridge_decoder_drain((*ptr.cast::<Decoder>()).inner) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_decoder_flush(ptr: *mut c_void) {
    if !ptr.is_null() {
        unsafe { bridge_decoder_flush((*ptr.cast::<Decoder>()).inner) };
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_decoder_send(
    ptr: *mut c_void,
    data: *const u8,
    len: usize,
    pts: i64,
) -> c_int {
    if ptr.is_null() || data.is_null() || len == 0 {
        return -1;
    }
    unsafe { bridge_decoder_send((*ptr.cast::<Decoder>()).inner, data, len, pts) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_decoder_receive(ptr: *mut c_void) -> *mut c_void {
    if ptr.is_null() {
        return ptr::null_mut();
    }
    let dec = unsafe { &*ptr.cast::<Decoder>() };
    let inner = unsafe { bridge_decoder_receive(dec.inner) };
    if inner.is_null() {
        return ptr::null_mut();
    }
    Box::into_raw(Box::new(Frame {
        inner,
        generation: dec.generation,
    }))
    .cast()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_frame_free(ptr: *mut c_void) {
    if !ptr.is_null() {
        drop(unsafe { Box::from_raw(ptr.cast::<Frame>()) });
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_frame_info(ptr: *mut c_void, info: *mut FrameInfo) {
    if ptr.is_null() || info.is_null() {
        return;
    }
    unsafe { bridge_frame_info((*ptr.cast::<Frame>()).inner, info) };
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_frame_generation(ptr: *mut c_void) -> u64 {
    if ptr.is_null() {
        return 0;
    }
    unsafe { (*ptr.cast::<Frame>()).generation }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_frame_ready(ptr: *mut c_void) -> c_int {
    if ptr.is_null() {
        return 0;
    }
    unsafe { bridge_frame_ready((*ptr.cast::<Frame>()).inner) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_frame_wait(ptr: *mut c_void) -> c_int {
    if ptr.is_null() {
        return -1;
    }
    unsafe { bridge_frame_wait((*ptr.cast::<Frame>()).inner) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_renderer_new(w: c_int, h: c_int, generation: u64) -> *mut c_void {
    if w <= 0 || h <= 0 || w > 16384 || h > 16384 {
        return ptr::null_mut();
    }
    let inner = unsafe { bridge_renderer_new(w, h) };
    if inner.is_null() {
        return ptr::null_mut();
    }
    Box::into_raw(Box::new(Renderer {
        inner,
        generation,
        width: w,
        height: h,
    }))
    .cast()
}
fn tile_fits(
    rw: i32,
    rh: i32,
    fw: i32,
    fh: i32,
    origin: i32,
    rows: i32,
    generation: u64,
    frame_generation: u64,
) -> bool {
    generation == frame_generation
        && fw > 0
        && fw <= rw
        && fh > 0
        && origin >= 0
        && rows > 0
        && rows <= fh
        && origin.checked_add(rows).is_some_and(|end| end <= rh)
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_renderer_upload(
    ptr: *mut c_void,
    frame: *mut c_void,
    origin: c_int,
    rows: c_int,
) -> c_int {
    if ptr.is_null() || frame.is_null() {
        return -1;
    }
    let renderer = unsafe { &*ptr.cast::<Renderer>() };
    let frame = unsafe { &*frame.cast::<Frame>() };
    let mut info = FrameInfo::default();
    unsafe { bridge_frame_info(frame.inner, &mut info) };
    if !tile_fits(
        renderer.width,
        renderer.height,
        info.width,
        info.height,
        origin,
        rows,
        renderer.generation,
        frame.generation,
    ) {
        return 1;
    }
    unsafe { bridge_renderer_upload(renderer.inner, frame.inner, origin, rows) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_renderer_draw(ptr: *mut c_void, w: c_int, h: c_int) -> c_int {
    if ptr.is_null() || w <= 0 || h <= 0 {
        return -1;
    }
    unsafe { bridge_renderer_draw((*ptr.cast::<Renderer>()).inner, w, h) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_renderer_free(ptr: *mut c_void) {
    if !ptr.is_null() {
        drop(unsafe { Box::from_raw(ptr.cast::<Renderer>()) });
    }
}

#[cfg(test)]
mod tests {
    use super::tile_fits;
    #[test]
    fn valid_padded_bottom_tile() {
        assert!(tile_fits(3840, 2160, 3840, 544, 1632, 528, 7, 7));
    }
    #[test]
    fn stale_generation_is_rejected_even_if_dimensions_match() {
        assert!(!tile_fits(3840, 2160, 3840, 544, 0, 544, 8, 7));
    }
    #[test]
    fn old_wider_canvas_is_rejected() {
        assert!(!tile_fits(1920, 1080, 3840, 544, 0, 544, 8, 8));
    }
    #[test]
    fn overflowing_tile_origin_is_rejected() {
        assert!(!tile_fits(3840, 2160, 3840, 544, i32::MAX, 544, 8, 8));
    }
}
