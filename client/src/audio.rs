use std::ffi::c_void;
unsafe extern "C" {
    fn iss_audio_new() -> *mut c_void;
    fn iss_audio_feed(ptr: *mut c_void, data: *const u8, len: u32) -> i32;
    fn iss_audio_free(ptr: *mut c_void);
}
struct Audio(*mut c_void);
impl Drop for Audio {
    fn drop(&mut self) {
        unsafe { iss_audio_free(self.0) }
    }
}
pub fn run(
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<(), String> {
    let audio = Audio(unsafe { iss_audio_new() });
    if audio.0.is_null() {
        return Err(
            "AAC-ELD requires libfdk-aac and PortAudio; install them or use --no-audio".into(),
        );
    }
    let mut frames = 0u64;
    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        match rx.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(bytes) => {
                let result = unsafe { iss_audio_feed(audio.0, bytes.as_ptr(), bytes.len() as u32) };
                if result < 0 {
                    return Err("AAC-ELD playback device/format failed".into());
                }
                frames += result as u64;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => break,
        }
    }
    eprintln!("native audio: {frames} PCM frames played");
    Ok(())
}
