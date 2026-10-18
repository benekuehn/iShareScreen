//! Receive encrypted datagrams independently of Python's GIL. No parsing or
//! authentication happens here: packets still pass through the SRTP verifier.
use std::collections::VecDeque;
use std::ffi::{c_int, c_void};
use std::io;
use std::net::UdpSocket;
use std::os::fd::BorrowedFd;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const BYTE_LIMIT: usize = 32 * 1024 * 1024;
struct Packets {
    queue: VecDeque<Vec<u8>>,
    bytes: usize,
    capacity: usize,
}
impl Packets {
    fn push(&mut self, packet: &[u8]) -> bool {
        if self.queue.len() >= self.capacity || self.bytes + packet.len() > BYTE_LIMIT {
            return false;
        }
        self.bytes += packet.len();
        self.queue.push_back(packet.to_vec());
        true
    }
    fn drain(&mut self, target: &mut [u8]) -> usize {
        let mut used = 0;
        while let Some(packet) = self.queue.front() {
            if used + 2 + packet.len() > target.len() {
                break;
            }
            let packet = self.queue.pop_front().unwrap();
            let size = packet.len();
            target[used..used + 2].copy_from_slice(&(size as u16).to_be_bytes());
            target[used + 2..used + 2 + size].copy_from_slice(&packet);
            self.bytes -= size;
            used += 2 + size;
        }
        used
    }
}
struct Shared {
    packets: Mutex<Packets>,
    ready: Condvar,
    stop: AtomicBool,
    dropped: AtomicU64,
}
struct Receiver {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}
impl Receiver {
    unsafe fn new(fd: c_int, capacity: usize) -> io::Result<Self> {
        // Duplicate ownership, not the socket's receive queue. Only this
        // worker reads it until shutdown; the original is retained for sends.
        let owned = unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned()?;
        let socket = UdpSocket::from(owned);
        socket.set_nonblocking(false)?;
        socket.set_read_timeout(Some(Duration::from_millis(100)))?;
        let shared = Arc::new(Shared {
            packets: Mutex::new(Packets {
                queue: VecDeque::new(),
                bytes: 0,
                capacity,
            }),
            ready: Condvar::new(),
            stop: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
        });
        let state = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("iss-udp-native".into())
            .spawn(move || {
                let mut buffer = [0u8; 65536];
                while !state.stop.load(Ordering::Relaxed) {
                    match socket.recv(&mut buffer) {
                        Ok(size) => {
                            let mut packets = state.packets.lock().unwrap();
                            if !packets.push(&buffer[..size]) {
                                state.dropped.fetch_add(1, Ordering::Relaxed);
                            }
                            drop(packets);
                            state.ready.notify_one();
                        }
                        Err(error)
                            if matches!(
                                error.kind(),
                                io::ErrorKind::WouldBlock
                                    | io::ErrorKind::TimedOut
                                    | io::ErrorKind::Interrupted
                            ) => {}
                        Err(_) => break,
                    }
                }
            })?;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }
}
impl Drop for Receiver {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.ready.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_udp_new(fd: c_int, capacity: usize) -> *mut c_void {
    if fd < 0 || capacity == 0 || capacity > 65536 {
        return std::ptr::null_mut();
    }
    match unsafe { Receiver::new(fd, capacity) } {
        Ok(receiver) => Box::into_raw(Box::new(receiver)).cast(),
        Err(_) => std::ptr::null_mut(),
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_udp_read(ptr: *mut c_void, data: *mut u8, capacity: usize) -> usize {
    if ptr.is_null() || data.is_null() || capacity < 65538 {
        return 0;
    }
    let receiver = unsafe { &*ptr.cast::<Receiver>() };
    let shared = &receiver.shared;
    let packets = shared.packets.lock().unwrap();
    let (mut packets, _) = shared
        .ready
        .wait_timeout_while(packets, Duration::from_millis(10), |packets| {
            packets.queue.is_empty() && !shared.stop.load(Ordering::Relaxed)
        })
        .unwrap();
    packets.drain(unsafe { std::slice::from_raw_parts_mut(data, capacity) })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_udp_dropped(ptr: *mut c_void) -> u64 {
    if ptr.is_null() {
        return 0;
    }
    unsafe { &*ptr.cast::<Receiver>() }
        .shared
        .dropped
        .load(Ordering::Relaxed)
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_udp_depth(ptr: *mut c_void) -> usize {
    if ptr.is_null() {
        return 0;
    }
    unsafe { &*ptr.cast::<Receiver>() }
        .shared
        .packets
        .lock()
        .unwrap()
        .queue
        .len()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iss_udp_free(ptr: *mut c_void) {
    if !ptr.is_null() {
        drop(unsafe { Box::from_raw(ptr.cast::<Receiver>()) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_queue_keeps_order_and_counts_bytes() {
        let mut queue = Packets {
            queue: VecDeque::new(),
            bytes: 0,
            capacity: 2,
        };
        assert!(queue.push(b"first"));
        assert!(queue.push(b"second"));
        assert!(!queue.push(b"overflow"));
        let mut output = [0; 15];
        assert_eq!(queue.drain(&mut output), 15);
        assert_eq!(&output, b"\0\x05first\0\x06second");
        assert_eq!(queue.bytes, 0);
    }
    #[test]
    fn short_batch_leaves_next_packet_intact() {
        let mut queue = Packets {
            queue: VecDeque::new(),
            bytes: 0,
            capacity: 2,
        };
        queue.push(b"first");
        queue.push(b"second");
        let mut output = [0; 7];
        assert_eq!(queue.drain(&mut output), 7);
        assert_eq!(queue.queue.front().unwrap(), b"second");
        assert_eq!(queue.bytes, 6);
    }
}
