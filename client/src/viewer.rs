//! Window events, network, decode and GL rendering have separate owners.
use crate::{
    gpu,
    transport::{self, AccessUnit, Command, Sockets},
    wire::{Negotiated, Result},
};
use isharescreen_native::presentation::*;
use isharescreen_protocol::activity::Activity;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};

#[repr(C)]
#[derive(Default)]
struct Event {
    kind: c_int,
    a: c_int,
    b: c_int,
    c: c_int,
    x: f64,
    y: f64,
}
unsafe extern "C" {
    fn iss_window_new() -> *mut c_void;
    fn iss_window_context(window: *mut c_void, acquire: c_int);
    fn iss_window_swap(window: *mut c_void);
    fn iss_window_state(window: *mut c_void, state: *mut c_int) -> c_int;
    fn iss_window_wait(seconds: f64);
    fn iss_window_wake();
    fn iss_window_event(window: *mut c_void, event: *mut Event) -> c_int;
    fn iss_window_free(window: *mut c_void);
    fn iss_window_display() -> *mut c_void;
    fn iss_window_surface(window: *mut c_void) -> *mut c_void;
    fn iss_window_cursor(
        window: *mut c_void,
        w: c_int,
        h: c_int,
        x: c_int,
        y: c_int,
        rgba: *const u8,
    );
    fn iss_window_clipboard(window: *mut c_void) -> *const c_char;
    fn iss_window_set_clipboard(window: *mut c_void, text: *const c_char);
    fn av_log_set_level(level: c_int);
    fn iss_process_cpu_seconds() -> f64;
}
struct View {
    geometry: [i32; 6],
    canvas: (u32, u32),
    frame: Option<(gpu::Frame, u64)>,
    error: Option<String>,
    activity: Activity,
    decoded: u64,
    presented: u64,
    skipped: u64,
}
struct Shared {
    view: Mutex<View>,
    changed: Condvar,
    start: Instant,
    stop: Arc<AtomicBool>,
    epoch: Arc<AtomicU64>,
    recover: Arc<AtomicBool>,
    active: Arc<AtomicU32>,
}
impl Shared {
    fn now(&self) -> u64 {
        self.start.elapsed().as_nanos() as u64
    }
    fn fail(&self, error: impl ToString) {
        self.view.lock().unwrap().error = Some(error.to_string());
        self.stop.store(true, Ordering::Relaxed);
        self.changed.notify_all();
        unsafe { iss_window_wake() };
    }
}
fn annex(nalus: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for nalu in nalus {
        out.extend([0, 0, 0, 1]);
        out.extend(nalu);
    }
    out
}
fn decode(shared: Arc<Shared>, aus: mpsc::Receiver<AccessUnit>) -> std::result::Result<(), String> {
    let mut decoder: Option<gpu::Decoder> = None;
    let mut source = None;
    let mut parameters: HashMap<u32, Vec<Vec<u8>>> = HashMap::new();
    let mut installed = Vec::new();
    let mut retired = std::collections::HashSet::new();
    let mut generation = 0;
    let mut epoch = 0;
    let mut waiting = true;
    let mut warned = Instant::now() - Duration::from_secs(2);
    while !shared.stop.load(Ordering::Relaxed) {
        let au = match aus.recv_timeout(Duration::from_millis(100)) {
            Ok(au) => au,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        };
        if retired.contains(&au.source) {
            continue;
        }
        let current = shared.epoch.load(Ordering::Relaxed);
        if au.epoch != current {
            continue;
        }
        if current != epoch {
            epoch = current;
            waiting = true;
        }
        for nalu in &au.nalus {
            let kind = (nalu[0] >> 1) & 63;
            if (32..=34).contains(&kind) {
                let p = parameters.entry(au.source).or_default();
                if !p.iter().any(|v| v == nalu) {
                    p.retain(|v| (v[0] >> 1) & 63 != kind);
                    p.push(nalu.clone());
                }
            }
        }
        parameters.retain(|ssrc, _| !retired.contains(ssrc));
        if parameters.len() > 8 {
            return Err("too many active video parameter generations".into());
        }
        let idr = au
            .nalus
            .iter()
            .any(|n| (16..=21).contains(&((n[0] >> 1) & 63)));
        if !au.nalus.iter().any(|n| (n[0] >> 1) & 63 < 32) {
            continue;
        }
        if source != Some(au.source) || parameters.get(&au.source).is_some_and(|p| *p != installed)
        {
            if !idr {
                if source.is_none() || source == Some(au.source) {
                    shared.recover.store(true, Ordering::Relaxed);
                }
                continue;
            }
            decoder = None;
            waiting = true;
        }
        if waiting && !idr {
            shared.recover.store(true, Ordering::Relaxed);
            continue;
        }
        if decoder.is_none() {
            let Some(params) = parameters.get(&au.source) else {
                shared.recover.store(true, Ordering::Relaxed);
                continue;
            };
            if !(32..=34).all(|kind| params.iter().any(|n| (n[0] >> 1) & 63 == kind)) {
                shared.recover.store(true, Ordering::Relaxed);
                continue;
            }
            generation += 1;
            shared.active.store(au.source, Ordering::Relaxed);
            decoder = Some(gpu::Decoder::new(&annex(params), generation)?);
            if let Some(old) = source {
                if old != au.source {
                    retired.insert(old);
                }
            }
            source = Some(au.source);
            installed = params.clone();
        }
        let d = decoder.as_mut().unwrap();
        let bytes = annex(&au.nalus);
        let result = (|| -> std::result::Result<Vec<gpu::Frame>, String> {
            let sent = d.send(&bytes, au.timestamp as i64)?;
            let mut frames = d.receive()?;
            if !sent {
                if !d.send(&bytes, au.timestamp as i64)? {
                    return Err("decoder remained full after output drain".into());
                }
                frames.extend(d.receive()?);
            }
            Ok(frames)
        })();
        match result {
            Ok(frames) => {
                waiting = false;
                for frame in frames {
                    let info = frame.info();
                    if info.error != 0 {
                        waiting = true;
                        shared.recover.store(true, Ordering::Relaxed);
                        continue;
                    }
                    let selected = {
                        let mut view = shared.view.lock().unwrap();
                        view.decoded += 1;
                        view.canvas = (info.width as u32, info.height as u32);
                        view.activity.take_output(shared.now())
                    };
                    if selected {
                        frame.wait()?;
                        let mut view = shared.view.lock().unwrap();
                        if view.frame.is_some() {
                            view.skipped += 1;
                        }
                        view.frame = Some((frame, generation));
                        drop(view);
                        shared.changed.notify_all();
                    }
                }
            }
            Err(error) => {
                waiting = true;
                shared.recover.store(true, Ordering::Relaxed);
                if warned.elapsed() > Duration::from_secs(1) {
                    eprintln!("native decoder recovery: {error}");
                    warned = Instant::now();
                }
            }
        }
    }
    Ok(())
}
fn render(
    shared: Arc<Shared>,
    window: usize,
    presentation: usize,
) -> std::result::Result<(), String> {
    let window = window as *mut c_void;
    unsafe { iss_window_context(window, 1) };
    let presentation = presentation as *mut c_void;
    let mut sequence = 0u64;
    let result = (|| {
        let mut renderer: Option<gpu::Renderer> = None;
        let mut canvas = (0, 0, 0);
        let mut size = (0, 0);
        while !shared.stop.load(Ordering::Relaxed) {
            let mut view = shared.view.lock().unwrap();
            let geometry = view.geometry;
            if geometry[3] == 0 || geometry[0] <= 0 || geometry[1] <= 0 {
                drop(view);
                renderer = None;
                canvas = (0, 0, 0);
                let view = shared.view.lock().unwrap();
                drop(
                    shared
                        .changed
                        .wait_timeout(view, Duration::from_millis(500))
                        .unwrap(),
                );
                continue;
            }
            let delay = view.activity.render_delay(shared.now());
            if delay > 0 {
                drop(
                    shared
                        .changed
                        .wait_timeout(view, Duration::from_nanos(delay.min(500_000_000)))
                        .unwrap(),
                );
                continue;
            }
            let frame = view.frame.take();
            let redraw = size != (geometry[0], geometry[1]);
            if frame.is_none() && !redraw {
                drop(
                    shared
                        .changed
                        .wait_timeout(view, Duration::from_millis(200))
                        .unwrap(),
                );
                continue;
            }
            drop(view);
            if let Some((frame, generation)) = frame {
                let info = frame.info();
                let dims = (info.width, info.height, generation);
                if dims != canvas {
                    drop(renderer.take());
                    renderer = Some(gpu::Renderer::new(info.width, info.height, generation)?);
                    canvas = dims;
                }
                renderer.as_mut().unwrap().upload(&frame)?;
            }
            if let Some(renderer) = &renderer {
                renderer.draw(geometry[0], geometry[1])?;
                sequence += 1;
                if !presentation.is_null() {
                    unsafe { iss_presentation_request(presentation, sequence, 0, 0) };
                }
                unsafe { iss_window_swap(window) };
                let mut view = shared.view.lock().unwrap();
                view.presented += 1;
                view.activity.submitted(shared.now());
                size = (geometry[0], geometry[1]);
            }
        }
        Ok(())
    })();
    unsafe { iss_window_context(window, 0) };
    result
}
fn key_symbol(key: i32) -> Option<u32> {
    Some(match key {
        256 => 0xff1b,
        257 => 0xff0d,
        258 => 0xff09,
        259 => 0xff08,
        260 => 0xff63,
        261 => 0xffff,
        262 => 0xff53,
        263 => 0xff51,
        264 => 0xff54,
        265 => 0xff52,
        266 => 0xff55,
        267 => 0xff56,
        268 => 0xff50,
        269 => 0xff57,
        280 => 0xffe5,
        290..=301 => 0xffbe + (key - 290) as u32,
        340 => 0xffe1,
        344 => 0xffe2,
        341 => 0xffeb,
        345 => 0xffec,
        342 => 0xffe9,
        346 => 0xffea,
        343 => 0xffeb,
        347 => 0xffec,
        _ => return None,
    })
}
fn key_message(down: bool, symbol: u32) -> Vec<u8> {
    let mut out = vec![4, u8::from(down), 0, 0];
    out.extend(symbol.to_be_bytes());
    out
}
fn send(tx: &transport::CommandTx, command: Command) -> Result<()> {
    tx.send(command)
}
pub fn run(
    n: Negotiated,
    sockets: Sockets,
    background: u32,
    auto_quit: f64,
    audio: bool,
) -> Result<()> {
    let activity = Activity::new(background).ok_or("background FPS must be between 1 and 60")?;
    unsafe { av_log_set_level(8) }; // FFmpeg diagnostics are summarized by the decode worker.
    let canvas = (n.width, n.height);
    let shared = Arc::new(Shared {
        view: Mutex::new(View {
            geometry: [1280, 720, 1, 1, 1280, 720],
            canvas,
            frame: None,
            error: None,
            activity,
            decoded: 0,
            presented: 0,
            skipped: 0,
        }),
        changed: Condvar::new(),
        start: Instant::now(),
        stop: sockets.stop.clone(),
        epoch: Arc::new(AtomicU64::new(0)),
        recover: Arc::new(AtomicBool::new(true)),
        active: Arc::new(AtomicU32::new(0)),
    });
    let updates = Arc::new(Mutex::new(crate::extensions::Updates::default()));
    let (tx, commands) = sockets.command_channel();
    let (aus, rx) = mpsc::sync_channel(64);
    let (audio_tx, audio_rx) = mpsc::sync_channel(16);
    let sound = if audio {
        let shared = shared.clone();
        Some(thread::spawn(move || {
            if let Err(error) = crate::audio::run(audio_rx, shared.stop.clone()) {
                shared.fail(error);
            }
        }))
    } else {
        None
    };
    let net = {
        let shared = shared.clone();
        let updates = updates.clone();
        thread::spawn(move || {
            if let Err(error) = transport::run(
                n,
                sockets,
                commands,
                aus,
                shared.epoch.clone(),
                shared.recover.clone(),
                shared.active.clone(),
                audio.then_some(audio_tx),
                updates,
            ) {
                shared.fail(error);
            }
            shared.stop.store(true, Ordering::Relaxed);
        })
    };
    let decoder = {
        let shared = shared.clone();
        thread::spawn(move || {
            if let Err(error) = decode(shared.clone(), rx) {
                shared.fail(error);
            }
        })
    };
    let window = unsafe { iss_window_new() };
    if window.is_null() {
        shared.stop.store(true, Ordering::Relaxed);
        let _ = net.join();
        let _ = decoder.join();
        return Err("could not create GLFW EGL/GLES3 window".into());
    }
    let presentation =
        unsafe { iss_presentation_new(iss_window_display(), iss_window_surface(window)) };
    let render = {
        let shared = shared.clone();
        let address = window as usize;
        let observer = presentation as usize;
        thread::spawn(move || {
            if let Err(error) = render(shared.clone(), address, observer) {
                shared.fail(error);
            }
        })
    };
    eprintln!(
        "Native Rust viewer connected: {}x{}, VAAPI/EGL, background cap {} fps",
        canvas.0, canvas.1, background
    );
    let result = (|| -> Result<()> {
        let mut held = HashMap::new();
        let mut buttons = 0u8;
        let mut point = (0u16, 0u16);
        let mut focused = true;
        let mut profile = Instant::now();
        let mut last = (0, 0);
        let mut last_cpu = unsafe { iss_process_cpu_seconds() };
        let mut clipboard = String::new();
        let mut clipboard_poll = Instant::now();
        let mut displayed = 0;
        let mut discarded = 0;
        while !shared.stop.load(Ordering::Relaxed) {
            if net.is_finished() || decoder.is_finished() || render.is_finished() {
                return Err("session worker exited unexpectedly".into());
            }
            if shared.start.elapsed() > Duration::from_secs(10)
                && shared.view.lock().unwrap().decoded == 0
            {
                return Err("host produced no decodable startup picture; reconnect the session (or use --reconnect)".into());
            }
            if !presentation.is_null() {
                unsafe { iss_presentation_pump(presentation) };
                let mut events = [PresentationEvent::default(); 256];
                let count = unsafe {
                    iss_presentation_read(presentation, events.as_mut_ptr(), events.len())
                };
                for event in &events[..count] {
                    if event.discarded != 0 {
                        discarded += 1;
                    } else if event.presented_ns != 0 {
                        displayed += 1;
                    }
                }
            }
            if auto_quit > 0.0 && shared.start.elapsed().as_secs_f64() >= auto_quit {
                break;
            }
            let mut geometry = [0i32; 6];
            if unsafe { iss_window_state(window, geometry.as_mut_ptr()) } != 0 {
                break;
            }
            let canvas = shared.view.lock().unwrap().canvas;
            {
                let mut updates = updates.lock().unwrap();
                if let Some(cursor) = updates.cursor.take() {
                    if let Some(c) = cursor {
                        unsafe {
                            iss_window_cursor(
                                window,
                                c.width as i32,
                                c.height as i32,
                                c.x as i32,
                                c.y as i32,
                                c.rgba.as_ptr(),
                            )
                        };
                    } else {
                        unsafe { iss_window_cursor(window, 0, 0, 0, 0, std::ptr::null()) };
                    }
                }
                if let Some(text) = updates.clipboard.take() {
                    if std::env::var("ISS_CLIP").as_deref() != Ok("0") {
                        if let Ok(value) = CString::new(text.clone()) {
                            unsafe { iss_window_set_clipboard(window, value.as_ptr()) };
                            clipboard = text;
                        }
                    }
                }
            }
            if geometry[2] != 0
                && clipboard_poll.elapsed() >= Duration::from_secs(1)
                && std::env::var("ISS_CLIP").as_deref() != Ok("0")
            {
                clipboard_poll = Instant::now();
                let text = unsafe { iss_window_clipboard(window) };
                if !text.is_null() {
                    let value = unsafe { CStr::from_ptr(text) };
                    if value.to_bytes().len() <= 1024 * 1024 {
                        let value = value.to_string_lossy();
                        if value != clipboard {
                            clipboard = value.into_owned();
                            send(&tx, Command::Clipboard(clipboard.clone()))?;
                        }
                    }
                }
            }
            let wait = {
                let mut view = shared.view.lock().unwrap();
                let old = view.geometry;
                view.geometry = geometry;
                view.activity
                    .window(geometry[2] != 0, geometry[3] != 0, shared.now());
                let wait = view.activity.event_wait(shared.now());
                if old != geometry {
                    shared.changed.notify_all();
                }
                wait
            };
            if focused && geometry[2] == 0 {
                for (_, symbol) in held.drain() {
                    send(&tx, Command::Wire(key_message(false, symbol)))?;
                }
                buttons = 0;
                send(&tx, Command::Pointer(0, point.0, point.1))?;
            }
            focused = geometry[2] != 0;
            let mut event = Event::default();
            while unsafe { iss_window_event(window, &mut event) } != 0 {
                shared.view.lock().unwrap().activity.input(shared.now());
                match event.kind {
                    1 => {
                        let fit = (geometry[4] as f64 / canvas.0 as f64)
                            .min(geometry[5] as f64 / canvas.1 as f64);
                        if fit > 0.0 {
                            point = (
                                ((event.x - (geometry[4] as f64 - canvas.0 as f64 * fit) / 2.0)
                                    / fit)
                                    .clamp(0.0, (canvas.0 - 1) as f64)
                                    as u16,
                                ((event.y - (geometry[5] as f64 - canvas.1 as f64 * fit) / 2.0)
                                    / fit)
                                    .clamp(0.0, (canvas.1 - 1) as f64)
                                    as u16,
                            );
                            send(&tx, Command::Pointer(buttons, point.0, point.1))?;
                        }
                    }
                    2 => {
                        let mask = match event.a {
                            0 => 1,
                            1 => 2,
                            2 => 4,
                            _ => 0,
                        };
                        if event.b == 0 {
                            buttons &= !mask;
                        } else {
                            buttons |= mask;
                        }
                        send(&tx, Command::Pointer(buttons, point.0, point.1))?;
                    }
                    3 => {
                        let mask = if event.y > 0.0 { 8 } else { 16 };
                        for _ in 0..(event.y.abs() * 12.0).round().min(48.0) as usize {
                            send(&tx, Command::Pointer(buttons | mask, point.0, point.1))?;
                            send(&tx, Command::Pointer(buttons, point.0, point.1))?;
                        }
                    }
                    4 => {
                        let symbol = if event.b == 0 {
                            held.remove(&event.a)
                        } else {
                            key_symbol(event.a).or_else(|| {
                                if event.c & 14 != 0
                                    && ((65..=90).contains(&event.a)
                                        || (48..=57).contains(&event.a)
                                        || event.a == 32)
                                {
                                    Some(if event.a >= 65 {
                                        event.a as u32 + 32
                                    } else {
                                        event.a as u32
                                    })
                                } else {
                                    None
                                }
                            })
                        };
                        if let Some(symbol) = symbol {
                            send(&tx, Command::Wire(key_message(event.b != 0, symbol)))?;
                            if event.b != 0 {
                                held.insert(event.a, symbol);
                            }
                        }
                    }
                    5 => {
                        let cp = event.a as u32;
                        let symbol = if cp <= 255 { cp } else { 0x01000000 | cp };
                        send(&tx, Command::Wire(key_message(true, symbol)))?;
                        send(&tx, Command::Wire(key_message(false, symbol)))?;
                    }
                    _ => {}
                }
            }
            if profile.elapsed() >= Duration::from_secs(5) {
                let v = shared.view.lock().unwrap();
                let dt = profile.elapsed().as_secs_f64();
                let s = v.activity.snapshot(shared.now());
                let cpu = unsafe { iss_process_cpu_seconds() };
                eprintln!(
                    "native Rust profile: mode={} cpu_percent={:.1} decoded_fps={:.1} submitted_fps={:.1} displayed_fps={:.1} discarded={} selected={} suppressed={} superseded={}",
                    ["active", "idle", "background", "minimized"][s.mode as usize],
                    (cpu - last_cpu) * 100.0 / dt,
                    (v.decoded - last.0) as f64 / dt,
                    (v.presented - last.1) as f64 / dt,
                    displayed as f64 / dt,
                    discarded,
                    s.outputs_selected,
                    s.outputs_suppressed,
                    v.skipped
                );
                last = (v.decoded, v.presented);
                last_cpu = cpu;
                displayed = 0;
                discarded = 0;
                profile = Instant::now();
            }
            unsafe { iss_window_wait(wait as f64 / 1e9) };
        }
        for (_, symbol) in held {
            let _ = send(&tx, Command::Wire(key_message(false, symbol)));
        }
        let _ = send(&tx, Command::Pointer(0, point.0, point.1));
        Ok(())
    })();
    // Close the socket worker and wake render/decode waits before releasing GL.
    thread::sleep(Duration::from_millis(20));
    shared.stop.store(true, Ordering::Relaxed);
    shared.changed.notify_all();
    let deadline = Instant::now() + Duration::from_secs(3);
    while (!render.is_finished()
        || !decoder.is_finished()
        || !net.is_finished()
        || sound.as_ref().is_some_and(|s| !s.is_finished()))
        && Instant::now() < deadline
    {
        unsafe { iss_window_wait(0.01) };
    }
    if !render.is_finished()
        || !decoder.is_finished()
        || !net.is_finished()
        || sound.as_ref().is_some_and(|s| !s.is_finished())
    {
        eprintln!("Session worker did not stop; exiting");
        std::process::exit(1);
    }
    let _ = net.join();
    let _ = decoder.join();
    if let Some(sound) = sound {
        let _ = sound.join();
    }
    let _ = render.join();
    unsafe {
        iss_presentation_free(presentation);
        iss_window_free(window)
    };
    if let Some(error) = shared.view.lock().unwrap().error.take() {
        return Err(error.into());
    }
    result
}
