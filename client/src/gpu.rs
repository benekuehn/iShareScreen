//! Owned, thread-confined GPU handles. Only completed frames cross threads.
use isharescreen_native::*;
use std::ffi::{CStr, CString, c_void};
pub fn error() -> String {
    unsafe { CStr::from_ptr(iss_error()).to_string_lossy().into_owned() }
}
pub struct Decoder(*mut c_void);
pub struct Frame(pub *mut c_void);
// AVFrame owns a reference-counted VA surface; decode completes before transfer.
unsafe impl Send for Frame {}
impl Drop for Frame {
    fn drop(&mut self) {
        unsafe { iss_frame_free(self.0) }
    }
}
impl Frame {
    pub fn info(&self) -> FrameInfo {
        let mut i = FrameInfo::default();
        unsafe { iss_frame_info(self.0, &mut i) };
        i
    }
    pub fn wait(&self) -> Result<(), String> {
        if unsafe { iss_frame_wait(self.0) } < 0 {
            Err(error())
        } else {
            Ok(())
        }
    }
}
impl Decoder {
    pub fn new(extra: &[u8], generation: u64) -> Result<Self, String> {
        let device =
            CString::new(std::env::var("ISS_VAAPI_DEVICE").unwrap_or("/dev/dri/renderD128".into()))
                .map_err(|e| e.to_string())?;
        let p =
            unsafe { iss_decoder_new(1, extra.as_ptr(), extra.len(), device.as_ptr(), generation) };
        if p.is_null() {
            Err(error())
        } else {
            Ok(Self(p))
        }
    }
    pub fn send(&mut self, data: &[u8], pts: i64) -> Result<bool, String> {
        match unsafe { iss_decoder_send(self.0, data.as_ptr(), data.len(), pts) } {
            0 => Ok(true),
            1 => Ok(false),
            _ => Err(error()),
        }
    }
    pub fn receive(&mut self) -> Result<Vec<Frame>, String> {
        let mut out = Vec::new();
        loop {
            let p = unsafe { iss_decoder_receive(self.0) };
            if p.is_null() {
                let e = error();
                return if e.is_empty() { Ok(out) } else { Err(e) };
            }
            out.push(Frame(p));
        }
    }
}
impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { iss_decoder_free(self.0) }
    }
}
pub struct Renderer(*mut c_void);
impl Renderer {
    pub fn new(w: i32, h: i32, generation: u64) -> Result<Self, String> {
        let p = unsafe { iss_renderer_new(w, h, generation) };
        if p.is_null() {
            Err(error())
        } else {
            Ok(Self(p))
        }
    }
    pub fn upload(&mut self, frame: &Frame) -> Result<(), String> {
        let info = frame.info();
        if unsafe { iss_renderer_upload(self.0, frame.0, 0, info.height) } != 0 {
            Err(error())
        } else {
            Ok(())
        }
    }
    pub fn draw(&self, w: i32, h: i32) -> Result<(), String> {
        if unsafe { iss_renderer_draw(self.0, w, h) } < 0 {
            Err(error())
        } else {
            Ok(())
        }
    }
}
impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe { iss_renderer_free(self.0) }
    }
}
