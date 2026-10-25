//! Transitional C ABI for the reference client. The standalone Rust client
//! uses the safe modules directly. Handles own keys; calls are serialized.
use crate::{control, hevc, srtp};
use std::cell::RefCell;
use std::collections::HashSet;
use std::ffi::{CString, c_char, c_int, c_void};
use std::ptr;
use std::sync::Mutex;

enum State {
    RtpRx(srtp::Receiver),
    RtpTx(srtp::Sender),
    Rtcp(srtp::Control),
    Control(control::Cipher),
}
struct Handle(Mutex<State>);
thread_local! {
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::new("").unwrap());
}
#[unsafe(no_mangle)]
pub extern "C" fn iss_protocol_error() -> *const c_char {
    LAST_ERROR.with(|error| error.borrow().as_ptr())
}

/// Returns ownership of an authenticated TCP fd; caller must close it.
/// Keys/passwords are never serialized to argv, files, diagnostics or stdout.
#[cfg(unix)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_auth_connect(
    host: *const u8,
    host_len: usize,
    port: u16,
    mode: c_int,
    user: *const u8,
    user_len: usize,
    password: *const u8,
    password_len: usize,
    key_out: *mut u8,
) -> c_int {
    use std::os::fd::IntoRawFd;
    if key_out.is_null() || !matches!(mode, 0 | 1) {
        return -1;
    }
    let strings = unsafe {
        (
            input(host, host_len, 1024),
            input(user, user_len, 512),
            input(password, password_len, 4096),
        )
    };
    let (Some(host), Some(user), Some(password)) = strings else {
        return -1;
    };
    let strings = (
        std::str::from_utf8(host),
        std::str::from_utf8(user),
        std::str::from_utf8(password),
    );
    let (Ok(host), Ok(user), Ok(password)) = strings else {
        return -1;
    };
    match crate::auth::connect(host, port, user, password, mode == 0) {
        Ok((stream, key)) => {
            unsafe {
                ptr::copy_nonoverlapping(key.as_ptr(), key_out, 16);
            }
            stream.into_raw_fd()
        }
        Err(error) => {
            let error = error.to_string().replace('\0', "");
            LAST_ERROR.with(|slot| *slot.borrow_mut() = CString::new(error).unwrap());
            -1
        }
    }
}

unsafe fn input<'a>(data: *const u8, len: usize, limit: usize) -> Option<&'a [u8]> {
    if len > limit {
        return None;
    }
    if len == 0 {
        return Some(&[]);
    }
    if data.is_null() {
        return None;
    }
    Some(unsafe { std::slice::from_raw_parts(data, len) })
}
#[unsafe(no_mangle)]
pub extern "C" fn iss_protocol_abi_version() -> u32 {
    2
}

/// kind: 0 SRTP receive, 1 SRTP send, 2 SRTCP, 3 enc1103 control.
/// Pointers must designate live buffers of their supplied lengths.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_crypto_new(
    kind: c_int,
    blob: *const u8,
    len: usize,
    wrapping_key: *const u8,
    wrapping_len: usize,
    ssrc: u32,
) -> *mut c_void {
    let Some(blob) = (unsafe { input(blob, len, 46) }) else {
        return ptr::null_mut();
    };
    let result = match kind {
        0 => srtp::Receiver::new(blob).map(State::RtpRx),
        1 => srtp::Sender::new(blob, ssrc).map(State::RtpTx),
        2 => srtp::Control::new(blob).map(State::Rtcp),
        3 => {
            let Some(key) = (unsafe { input(wrapping_key, wrapping_len, 16) }) else {
                return ptr::null_mut();
            };
            control::Cipher::new(blob, key).map(State::Control)
        }
        _ => return ptr::null_mut(),
    };
    result.map_or(ptr::null_mut(), |state| {
        Box::into_raw(Box::new(Handle(Mutex::new(state)))).cast()
    })
}

/// operation: 0 decrypt/unprotect, 1 encrypt/protect. Returns output length,
/// or -1 on rejection. RTP receive writes header length to metadata; RTP send
/// reads the marker/payload-type byte from it. No partial output is published.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_crypto_transform(
    handle: *mut c_void,
    operation: c_int,
    data: *const u8,
    len: usize,
    output: *mut u8,
    capacity: usize,
    metadata: *mut u32,
) -> isize {
    if handle.is_null() || output.is_null() || operation < 0 || operation > 1 {
        return -1;
    }
    let Some(data) = (unsafe { input(data, len, 65535) }) else {
        return -1;
    };
    let handle = unsafe { &*handle.cast::<Handle>() };
    let Ok(mut state) = handle.0.lock() else {
        return -1;
    };
    let needed = len
        + match (&*state, operation) {
            (State::RtpTx(_), 1) => 22,
            (State::Rtcp(_), 1) => 14,
            (State::Control(_), 1) => 48,
            _ => 0,
        };
    if capacity < needed {
        return -1;
    }
    let mut header_len = 0;
    let result = match (&mut *state, operation) {
        (State::RtpRx(receiver), 0) => receiver.unprotect(data).map(|packet| {
            header_len = packet.header.len() as u32;
            [packet.header, packet.payload].concat()
        }),
        (State::RtpTx(sender), 1) => {
            let byte = if metadata.is_null() {
                101
            } else {
                (unsafe { *metadata }) as u8
            };
            sender.protect(data, byte & 127, byte & 128 != 0)
        }
        (State::Rtcp(control), 0) => control.unprotect(data),
        (State::Rtcp(control), 1) => control.protect(data),
        (State::Control(control), 0) => control.decrypt(data),
        (State::Control(control), 1) => control.encrypt(data),
        _ => return -1,
    };
    match result {
        Ok(bytes) if bytes.len() <= capacity => {
            unsafe {
                ptr::copy_nonoverlapping(bytes.as_ptr(), output, bytes.len());
            }
            if !metadata.is_null() && matches!(*state, State::RtpRx(_)) {
                unsafe {
                    *metadata = header_len;
                }
            }
            bytes.len() as isize
        }
        _ => -1,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_crypto_sources(
    handle: *mut c_void,
    out: *mut srtp::SourceReport,
    capacity: usize,
) -> usize {
    if handle.is_null() || out.is_null() {
        return 0;
    }
    let handle = unsafe { &*handle.cast::<Handle>() };
    let Ok(state) = handle.0.lock() else {
        return 0;
    };
    let State::RtpRx(receiver) = &*state else {
        return 0;
    };
    let sources = receiver.states();
    let count = capacity.min(sources.len());
    unsafe {
        ptr::copy_nonoverlapping(sources.as_ptr(), out, count);
    }
    count
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_crypto_keep_sources(
    handle: *mut c_void,
    sources: *const u32,
    count: usize,
) {
    if handle.is_null() || count > 64 || (count > 0 && sources.is_null()) {
        return;
    }
    let keep: HashSet<u32> = if count == 0 {
        HashSet::new()
    } else {
        unsafe { std::slice::from_raw_parts(sources, count) }
            .iter()
            .copied()
            .collect()
    };
    let handle = unsafe { &*handle.cast::<Handle>() };
    if let Ok(mut state) = handle.0.lock() {
        if let State::RtpRx(receiver) = &mut *state {
            receiver.forget_except(&keep);
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_crypto_control_key(handle: *mut c_void, out: *mut u8) -> c_int {
    if handle.is_null() || out.is_null() {
        return -1;
    }
    let handle = unsafe { &*handle.cast::<Handle>() };
    let Ok(state) = handle.0.lock() else {
        return -1;
    };
    if let State::Control(control) = &*state {
        unsafe {
            ptr::copy_nonoverlapping(control.cbc_key().as_ptr(), out, 16);
        }
        0
    } else {
        -1
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_crypto_free(handle: *mut c_void) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle.cast::<Handle>()) });
    }
}

/// Input consists of u16 BE length-prefixed RTP payloads; output is u32 BE
/// length-prefixed NAL units. Compressed bytes only, never decoded pixels.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_hevc_reassemble(
    data: *const u8,
    len: usize,
    donl: c_int,
    out: *mut u8,
    capacity: usize,
) -> isize {
    if out.is_null() || capacity < len.saturating_mul(2).saturating_add(16) {
        return -1;
    }
    let Some(data) = (unsafe { input(data, len, 16 * 1024 * 1024) }) else {
        return -1;
    };
    let mut payloads = Vec::new();
    let mut pos = 0;
    while pos + 2 <= data.len() {
        let size = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
        pos += 2;
        if size < 2 || pos + size > data.len() || payloads.len() >= 8192 {
            return -1;
        }
        payloads.push(&data[pos..pos + size]);
        pos += size;
    }
    if pos != data.len() {
        return -1;
    }
    let nalus = if donl & 2 != 0 {
        hevc::harvest_parameters(&payloads, donl & 1 != 0)
    } else {
        hevc::reassemble(&payloads, donl & 1 != 0)
    };
    let Ok(nalus) = nalus else {
        return -1;
    };
    let size: usize = nalus.iter().map(|nalu| 4 + nalu.len()).sum();
    if size > capacity {
        return -1;
    }
    let mut offset = 0;
    for nalu in nalus {
        unsafe {
            ptr::copy_nonoverlapping(
                (nalu.len() as u32).to_be_bytes().as_ptr(),
                out.add(offset),
                4,
            );
            ptr::copy_nonoverlapping(nalu.as_ptr(), out.add(offset + 4), nalu.len());
        }
        offset += 4 + nalu.len();
    }
    offset as isize
}
