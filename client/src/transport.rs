//! Authenticated, bounded RTP ordering and RTCP feedback; no UI or GPU calls.
use crate::wire::{Negotiated, Result};
use isharescreen_protocol::{hevc, srtp};
use std::collections::{BTreeMap, HashMap};
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::os::{fd::AsRawFd, unix::net::UnixDatagram};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    mpsc::{self, Receiver, SyncSender},
};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}
unsafe extern "C" {
    fn poll(fds: *mut PollFd, count: usize, timeout: i32) -> i32;
}
fn wait_fds(tcp: i32, wake: i32, timeout: i32) -> bool {
    let mut fds = [
        PollFd {
            fd: tcp,
            events: 1,
            revents: 0,
        },
        PollFd {
            fd: wake,
            events: 1,
            revents: 0,
        },
    ];
    unsafe { poll(fds.as_mut_ptr(), 2, timeout) };
    fds[0].revents != 0
}

pub struct Datagram {
    pub video: bool,
    pub bytes: Vec<u8>,
}
pub struct Sockets {
    pub video: UdpSocket,
    pub control: UdpSocket,
    video_dest: SocketAddr,
    control_dest: SocketAddr,
    pub packets: Receiver<Datagram>,
    pub stop: Arc<AtomicBool>,
    workers: Vec<thread::JoinHandle<()>>,
    pub dropped: Arc<AtomicU64>,
    bytes: Arc<AtomicU64>,
    wake_rx: UnixDatagram,
    wake_tx: Arc<UnixDatagram>,
}
impl Sockets {
    pub fn open(host: &str, port: u16) -> Result<Self> {
        let ip = (host, port)
            .to_socket_addrs()?
            .find(|a| a.is_ipv4())
            .ok_or("host has no IPv4 address")?
            .ip();
        let control = UdpSocket::bind(("0.0.0.0", port))?;
        let video = UdpSocket::bind(("0.0.0.0", port.checked_add(1).ok_or("invalid video port")?))?;
        let control_dest = SocketAddr::new(ip, port);
        let video_dest = SocketAddr::new(ip, port + 1);
        let (tx, packets) = mpsc::sync_channel(16384);
        let (wake_tx, wake_rx) = UnixDatagram::pair()?;
        wake_tx.set_nonblocking(true)?;
        wake_rx.set_nonblocking(true)?;
        let wake_tx = Arc::new(wake_tx);
        let stop = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicU64::new(0));
        let bytes = Arc::new(AtomicU64::new(0));
        let mut workers = Vec::new();
        for (is_video, socket, peer) in [
            (true, video.try_clone()?, video_dest),
            (false, control.try_clone()?, control_dest),
        ] {
            socket.set_read_timeout(Some(Duration::from_millis(100)))?;
            let tx = tx.clone();
            let stop = stop.clone();
            let dropped = dropped.clone();
            let bytes = bytes.clone();
            let wake = wake_tx.clone();
            workers.push(thread::spawn(move || {
                let mut buffer = [0u8; 65536];
                while !stop.load(Ordering::Relaxed) {
                    match socket.recv_from(&mut buffer) {
                        Ok((n, from)) => {
                            if from != peer {
                                continue;
                            }
                            if bytes
                                .try_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                                    v.checked_add(n as u64).filter(|v| *v <= 32 * 1024 * 1024)
                                })
                                .is_err()
                            {
                                dropped.fetch_add(1, Ordering::Relaxed);
                                continue;
                            }
                            if tx
                                .try_send(Datagram {
                                    video: is_video,
                                    bytes: buffer[..n].to_vec(),
                                })
                                .is_err()
                            {
                                bytes.fetch_sub(n as u64, Ordering::Relaxed);
                                dropped.fetch_add(1, Ordering::Relaxed);
                            }
                            let _ = wake.send(&[0]);
                        }
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock
                                    | std::io::ErrorKind::TimedOut
                                    | std::io::ErrorKind::Interrupted
                                    | std::io::ErrorKind::ConnectionRefused
                            ) => {}
                        Err(_) => break,
                    }
                }
            }));
        }
        // Open NAT/firewall paths before the host starts its initial burst.
        control.send_to(&[0], control_dest)?;
        video.send_to(&[0], video_dest)?;
        Ok(Self {
            video,
            control,
            video_dest,
            control_dest,
            packets,
            stop,
            workers,
            dropped,
            bytes,
            wake_rx,
            wake_tx,
        })
    }
    pub fn command_channel(&self) -> (CommandTx, Receiver<Command>) {
        let (tx, rx) = mpsc::sync_channel(512);
        (
            CommandTx {
                tx,
                wake: self.wake_tx.clone(),
            },
            rx,
        )
    }
}
impl Drop for Sockets {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}
pub enum Command {
    Wire(Vec<u8>),
    Pointer(u8, u16, u16),
    Clipboard(String),
}
pub struct CommandTx {
    tx: SyncSender<Command>,
    wake: Arc<UnixDatagram>,
}
impl CommandTx {
    pub fn send(&self, command: Command) -> Result<()> {
        self.tx
            .try_send(command)
            .map_err(|_| "input queue full or session stopped")?;
        let _ = self.wake.send(&[0]);
        Ok(())
    }
}
struct Permit {
    used: Arc<AtomicU64>,
    bytes: u64,
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.used.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}
pub struct AccessUnit {
    pub source: u32,
    pub timestamp: u32,
    pub epoch: u64,
    pub nalus: Vec<Vec<u8>>,
    _permit: Option<Permit>,
}
fn picture_lost(source: u32, active: &AtomicU32, epoch: &AtomicU64, recover: &AtomicBool) {
    let current = active.load(Ordering::Relaxed);
    if current == 0 || source == current {
        epoch.fetch_add(1, Ordering::Relaxed);
        recover.store(true, Ordering::Relaxed);
    }
}
fn access_unit(
    source: u32,
    timestamp: u32,
    epoch: u64,
    nalus: Vec<Vec<u8>>,
    used: &Arc<AtomicU64>,
) -> Option<AccessUnit> {
    let bytes = nalus.iter().map(|n| n.len() as u64).sum();
    used.try_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
        v.checked_add(bytes).filter(|v| *v <= 32 * 1024 * 1024)
    })
    .ok()?;
    Some(AccessUnit {
        source,
        timestamp,
        epoch,
        nalus,
        _permit: Some(Permit {
            used: used.clone(),
            bytes,
        }),
    })
}
struct Group {
    born: Instant,
    packets: BTreeMap<i64, Vec<u8>>,
    marker: Option<i64>,
    timestamp: u32,
    bytes: usize,
    nacks: u8,
    last_nack: Option<Instant>,
}
#[derive(Default)]
struct Source {
    highest: Option<i64>,
    groups: BTreeMap<i64, Group>,
    marker: Option<i64>,
    received: u64,
    base: i64,
    expected_prior: u64,
    received_prior: u64,
    jitter: f64,
    transit: Option<(Instant, u32)>,
    sr: Option<(u32, Instant)>,
}
fn extend(seq: u16, highest: i64) -> i64 {
    highest + seq.wrapping_sub(highest as u16) as i16 as i64
}
impl Source {
    fn observe(&mut self, p: &srtp::Packet, now: Instant) {
        let seq = self
            .highest
            .map_or(p.sequence as i64, |h| extend(p.sequence, h));
        if self.highest.is_none() {
            self.base = seq;
        }
        self.highest = Some(self.highest.map_or(seq, |h| h.max(seq)));
        self.received += 1;
        if let Some((arrival, timestamp)) = self.transit {
            let delta = now.duration_since(arrival).as_secs_f64() * 90000.0
                - p.timestamp.wrapping_sub(timestamp) as i32 as f64;
            self.jitter += (delta.abs() - self.jitter) / 16.0;
        }
        self.transit = Some((now, p.timestamp));
        if self.marker.is_some_and(|m| seq <= m) {
            return;
        }
        let key = self
            .groups
            .iter()
            .find(|(_, g)| g.timestamp == p.timestamp)
            .map(|(k, _)| *k)
            .unwrap_or(seq);
        let group = self.groups.entry(key).or_insert_with(|| Group {
            born: now,
            packets: BTreeMap::new(),
            marker: None,
            timestamp: p.timestamp,
            bytes: 0,
            nacks: 0,
            last_nack: None,
        });
        if p.marker {
            group.marker = Some(seq);
        }
        if group.bytes < 16 * 1024 * 1024 && group.packets.len() < 8192 {
            group.packets.entry(seq).or_insert_with(|| {
                group.bytes += p.payload.len();
                p.payload.clone()
            });
        }
    }
    fn next(&mut self, now: Instant) -> Option<std::result::Result<(u32, Vec<Vec<u8>>), ()>> {
        let (&key, g) = self.groups.first_key_value()?;
        let first = *g.packets.first_key_value()?.0;
        // Parameter-only APs in Apple's startup burst need not set M. They
        // are configuration, not a lost reference picture. Consume them now
        // so their timeout cannot invalidate a subsequently queued IDR.
        if g.marker.is_none() {
            let packets: Vec<&[u8]> = g.packets.values().map(Vec::as_slice).collect();
            if let Ok(nalus) = hevc::reassemble(&packets, false) {
                if !nalus.is_empty()
                    && nalus
                        .iter()
                        .all(|n| (32..=34).contains(&((n[0] >> 1) & 63)))
                {
                    let g = self.groups.remove(&key).unwrap();
                    self.marker = g.packets.last_key_value().map(|(s, _)| *s);
                    return Some(Ok((g.timestamp, nalus)));
                }
            }
        }
        let valid = g
            .marker
            .is_some_and(|end| end - first + 1 == g.packets.len() as i64)
            && self.marker.is_none_or(|m| first == m + 1);
        if valid {
            let packets: Vec<&[u8]> = g.packets.values().map(Vec::as_slice).collect();
            if let Ok(nalus) = hevc::reassemble(&packets, false) {
                let g = self.groups.remove(&key).unwrap();
                self.marker = g.marker;
                return Some(Ok((g.timestamp, nalus)));
            }
        }
        if now.duration_since(g.born) >= Duration::from_millis(75) || self.groups.len() > 32 {
            let g = self.groups.remove(&key).unwrap();
            self.marker = g
                .marker
                .or_else(|| g.packets.last_key_value().map(|(s, _)| *s));
            return Some(Err(()));
        }
        None
    }
    fn missing(&mut self, now: Instant) -> Vec<u16> {
        let Some(mut entry) = self.groups.first_entry() else {
            return Vec::new();
        };
        let g = entry.get_mut();
        if g.nacks >= 3
            || g.last_nack
                .is_some_and(|t| now.duration_since(t) < Duration::from_millis(20))
        {
            return Vec::new();
        }
        let start = self
            .marker
            .map_or(*g.packets.first_key_value().unwrap().0, |m| m + 1);
        let end = g.marker.unwrap_or(*g.packets.last_key_value().unwrap().0);
        if end - start > 1024 {
            return Vec::new();
        }
        let missing: Vec<u16> = (start..=end)
            .filter(|seq| !g.packets.contains_key(seq))
            .map(|seq| seq as u16)
            .collect();
        if !missing.is_empty() {
            g.nacks += 1;
            g.last_nack = Some(now);
        }
        missing
    }
    fn report(&mut self, ssrc: u32, now: Instant) -> Vec<u8> {
        let highest = self.highest.unwrap_or(0);
        let expected = (highest - self.base + 1).max(0) as u64;
        let interval = expected.saturating_sub(self.expected_prior);
        let received = self.received.saturating_sub(self.received_prior);
        let fraction = if interval > 0 {
            ((interval.saturating_sub(received) * 256) / interval).min(255)
        } else {
            0
        };
        let loss = (expected as i64 - self.received as i64).clamp(-8388608, 8388607);
        self.expected_prior = expected;
        self.received_prior = self.received;
        let (lsr, dlsr) = self.sr.map_or((0, 0), |(s, t)| {
            (s, (now.duration_since(t).as_secs_f64() * 65536.) as u32)
        });
        let mut out = Vec::new();
        for n in [
            ssrc,
            ((fraction as u32) << 24) | (loss as u32 & 0xffffff),
            highest as u32,
            self.jitter as u32,
            lsr,
            dlsr,
        ] {
            out.extend(n.to_be_bytes());
        }
        out
    }
}
fn header(fmt: u8, kind: u8, sender: u32, target: Option<u32>, body: &[u8]) -> Vec<u8> {
    let mut out = vec![0x80 | fmt, kind, 0, 0];
    out.extend(sender.to_be_bytes());
    if let Some(target) = target {
        out.extend(target.to_be_bytes());
    }
    out.extend(body);
    let len = (out.len() / 4 - 1) as u16;
    out[2..4].copy_from_slice(&len.to_be_bytes());
    out
}
fn fir(sender: u32, target: u32, seq: u8) -> Vec<u8> {
    let mut body = target.to_be_bytes().to_vec();
    body.extend([seq, 0, 0, 0]);
    let mut out = header(0, 201, sender, None, &[]);
    out.extend(header(4, 206, sender, Some(0), &body));
    out.extend(header(1, 206, sender, Some(target), &[]));
    out.extend(header(0, 192, target, None, &[]));
    out
}
fn nack(sender: u32, target: u32, missing: &[u16]) -> Vec<u8> {
    let mut body = Vec::new();
    let mut i = 0;
    while i < missing.len() {
        let pid = missing[i];
        let mut mask = 0u16;
        i += 1;
        while i < missing.len() {
            let diff = missing[i].wrapping_sub(pid);
            if !(1..=16).contains(&diff) {
                break;
            }
            mask |= 1 << (diff - 1);
            i += 1;
        }
        body.extend(pid.to_be_bytes());
        body.extend(mask.to_be_bytes());
    }
    header(1, 205, sender, Some(target), &body)
}
fn sr(sender: u32) -> Vec<u8> {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let mut body = Vec::new();
    for n in [
        (t.as_secs() + 2208988800) as u32,
        ((t.subsec_nanos() as u64 * 4294967296) / 1000000000) as u32,
        (t.as_secs_f64() * 90000.) as u32,
        0,
        0,
    ] {
        body.extend(n.to_be_bytes());
    }
    header(0, 200, sender, None, &body)
}
pub fn run(
    mut n: Negotiated,
    sockets: Sockets,
    commands: Receiver<Command>,
    aus: SyncSender<AccessUnit>,
    epoch: Arc<AtomicU64>,
    recover: Arc<AtomicBool>,
    active: Arc<AtomicU32>,
    audio: Option<SyncSender<Vec<u8>>>,
    updates: Arc<Mutex<crate::extensions::Updates>>,
) -> Result<()> {
    n.link
        .socket
        .set_read_timeout(Some(Duration::from_millis(1)))?;
    n.link
        .socket
        .set_write_timeout(Some(Duration::from_millis(250)))?;
    let mut receiver = srtp::Receiver::new(&*n.keys.video_receive)?;
    let mut feedback = srtp::Control::new(&*n.keys.video_send)?;
    let rtcp = srtp::Control::new(&*n.keys.video_receive)?;
    let mut heartbeat = srtp::Sender::new(&*n.keys.audio_send, n.audio_ssrc)?;
    let mut audio_rx = srtp::Receiver::new(&*n.keys.audio_receive)?;
    let mut sources: HashMap<u32, Source> = HashMap::new();
    let mut periodic = Instant::now();
    let mut retry = Instant::now();
    let mut request = Instant::now() - Duration::from_secs(2);
    let mut fir_seq = 0u8;
    let mut dropped = 0;
    let mut profile = Instant::now();
    let mut assembled = 0u64;
    let mut expired = 0u64;
    let mut keyframes = 0u64;
    let mut messages = std::mem::take(&mut n.pending);
    let au_bytes = Arc::new(AtomicU64::new(0));
    let mut extensions = crate::extensions::Extensions::default();
    let clipboard = std::env::var("ISS_CLIP").as_deref() != Ok("0");
    if clipboard {
        n.link.send(&[0x15, 0, 0, 1, 0, 0, 0, 0])?;
        n.link.send(&[0x0b, 0, 0, 0, 0, 0, 0, 0])?;
    }
    while !sockets.stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        for command in commands.try_iter() {
            match command {
                Command::Wire(bytes) => n.link.send(&bytes)?,
                Command::Pointer(button, x, y) => n.link.pointer(button, x, y)?,
                Command::Clipboard(text) => {
                    if clipboard {
                        if let Some(bytes) = crate::extensions::clipboard_send(&text) {
                            for chunk in bytes.chunks(60000) {
                                n.link.send(chunk)?;
                            }
                        }
                    }
                }
            }
        }
        if wait_fds(n.link.socket.as_raw_fd(), sockets.wake_rx.as_raw_fd(), 0) {
            messages.extend(n.link.poll()?);
        }
        let mut wake = [0u8; 256];
        while sockets.wake_rx.recv(&mut wake).is_ok() {}
        for message in messages.drain(..) {
            if let Some(reply) = extensions.consume(&message, &mut updates.lock().unwrap()) {
                if clipboard {
                    n.link.send(&reply)?;
                }
            }
        }
        for _ in 0..4096 {
            let Ok(packet) = sockets.packets.try_recv() else {
                break;
            };
            sockets
                .bytes
                .fetch_sub(packet.bytes.len() as u64, Ordering::Relaxed);
            if !packet.video {
                if let Some(audio) = &audio {
                    if let Ok(packet) = audio_rx.unprotect(&packet.bytes) {
                        let _ = audio.try_send(packet.payload);
                    }
                }
                continue;
            }
            if packet
                .bytes
                .get(1)
                .is_some_and(|pt| (192..=223).contains(pt))
            {
                if let Ok(plain) = rtcp.unprotect(&packet.bytes) {
                    let mut pos = 0;
                    while pos + 4 <= plain.len() {
                        let size =
                            (u16::from_be_bytes([plain[pos + 2], plain[pos + 3]]) as usize + 1) * 4;
                        if size < 4 || pos + size > plain.len() {
                            break;
                        }
                        if plain[pos + 1] == 200 && size >= 28 {
                            let source = u32::from_be_bytes(plain[pos + 4..pos + 8].try_into()?);
                            let seconds = u32::from_be_bytes(plain[pos + 8..pos + 12].try_into()?);
                            let fraction =
                                u32::from_be_bytes(plain[pos + 12..pos + 16].try_into()?);
                            if let Some(s) = sources.get_mut(&source) {
                                s.sr = Some((seconds << 16 | fraction >> 16, now));
                            }
                        }
                        pos += size;
                    }
                }
                continue;
            }
            if let Ok(packet) = receiver.unprotect(&packet.bytes) {
                if sources.len() >= 8 && !sources.contains_key(&packet.ssrc) {
                    if let Some(old) = sources
                        .iter()
                        .filter(|(_, s)| {
                            s.transit
                                .is_none_or(|(t, _)| now.duration_since(t) > Duration::from_secs(2))
                        })
                        .min_by_key(|(_, s)| s.transit.map(|(t, _)| t))
                        .map(|(source, _)| *source)
                    {
                        sources.remove(&old);
                        receiver.forget_except(&sources.keys().copied().collect());
                    } else {
                        continue;
                    }
                }
                let parameters = hevc::harvest_parameters(&[&packet.payload], false)?;
                if !parameters.is_empty() {
                    if let Some(au) = access_unit(
                        packet.ssrc,
                        packet.timestamp,
                        epoch.load(Ordering::Relaxed),
                        parameters,
                        &au_bytes,
                    ) {
                        let _ = aus.try_send(au);
                    }
                }
                let source = sources.entry(packet.ssrc).or_default();
                source.observe(&packet, now);
                if sources
                    .values()
                    .flat_map(|s| s.groups.values())
                    .map(|g| g.bytes)
                    .sum::<usize>()
                    > 32 * 1024 * 1024
                {
                    for s in sources.values_mut() {
                        s.groups.clear();
                        s.marker = None;
                    }
                    epoch.fetch_add(1, Ordering::Relaxed);
                    recover.store(true, Ordering::Relaxed);
                }
            }
        }
        for (&source, state) in &mut sources {
            while let Some(result) = state.next(now) {
                match result {
                    Ok((timestamp, nalus)) => {
                        assembled += 1;
                        if nalus
                            .iter()
                            .any(|n| (16..=21).contains(&((n[0] >> 1) & 63)))
                        {
                            keyframes += 1;
                        }
                        let accepted = access_unit(
                            source,
                            timestamp,
                            epoch.load(Ordering::Relaxed),
                            nalus,
                            &au_bytes,
                        )
                        .is_some_and(|au| aus.try_send(au).is_ok());
                        if !accepted {
                            epoch.fetch_add(1, Ordering::Relaxed);
                            recover.store(true, Ordering::Relaxed);
                        }
                    }
                    Err(()) => {
                        expired += 1;
                        picture_lost(source, &active, &epoch, &recover);
                    }
                }
            }
            if now.duration_since(retry) >= Duration::from_millis(20) {
                let missing = state.missing(now);
                if !missing.is_empty() {
                    sockets.video.send_to(
                        &feedback.protect(&nack(n.video_ssrc, source, &missing))?,
                        sockets.video_dest,
                    )?;
                }
            }
        }
        if now.duration_since(retry) >= Duration::from_millis(20) {
            retry = now;
        }
        let new_dropped = sockets.dropped.load(Ordering::Relaxed);
        if new_dropped != dropped {
            dropped = new_dropped;
            epoch.fetch_add(1, Ordering::Relaxed);
            recover.store(true, Ordering::Relaxed);
        }
        if recover.load(Ordering::Relaxed)
            && now.duration_since(request) >= Duration::from_millis(500)
        {
            recover.store(false, Ordering::Relaxed);
            request = now;
            fir_seq = fir_seq.wrapping_add(1);
            let current = active.load(Ordering::Relaxed);
            let targets: Vec<u32> = if current != 0 {
                vec![current]
            } else {
                sources
                    .iter()
                    .max_by_key(|(_, s)| s.transit.map(|(t, _)| t))
                    .map(|(source, _)| vec![*source])
                    .unwrap_or_default()
            };
            for source in targets {
                sockets.video.send_to(
                    &feedback.protect(&fir(n.video_ssrc, source, fir_seq))?,
                    sockets.video_dest,
                )?;
            }
        }
        if now.duration_since(periodic) >= Duration::from_millis(500) {
            periodic = now;
            sockets.control.send_to(
                &heartbeat.protect(&[0, 0x68, 0x34, 0], 101, false)?,
                sockets.control_dest,
            )?;
            let mut blocks = Vec::new();
            for (&source, state) in &mut sources {
                blocks.extend(state.report(source, now));
            }
            let mut report = sr(n.video_ssrc);
            report.extend(header(
                sources.len() as u8,
                201,
                n.video_ssrc,
                None,
                &blocks,
            ));
            sockets
                .video
                .send_to(&feedback.protect(&report)?, sockets.video_dest)?;
            n.link.send(&crate::wire::fbu(false))?;
        }
        if profile.elapsed() >= Duration::from_secs(5) {
            eprintln!(
                "native transport: sources={} assembled={} keyframes={} expired={} udp_dropped={} queued_bytes={}",
                sources.len(),
                assembled,
                keyframes,
                expired,
                dropped,
                au_bytes.load(Ordering::Relaxed)
            );
            profile = Instant::now();
        }
        // Socket readiness and input wake this wait immediately; quiet sessions
        // have no millisecond polling loop. Repair deadlines remain bounded.
        if sockets.bytes.load(Ordering::Relaxed) == 0 {
            let ms = if sources.values().any(|s| !s.groups.is_empty()) {
                10
            } else {
                (500 - periodic.elapsed().as_millis().min(499)) as i32
            };
            wait_fds(n.link.socket.as_raw_fd(), sockets.wake_rx.as_raw_fd(), ms);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn packet(seq: u16, marker: bool, payload: &[u8]) -> srtp::Packet {
        srtp::Packet {
            header: vec![],
            payload: payload.to_vec(),
            ssrc: 1,
            sequence: seq,
            timestamp: 10,
            marker,
        }
    }
    #[test]
    fn reordered_fragment_and_rollover_are_reassembled_before_deadline() {
        let t = Instant::now();
        let mut s = Source::default();
        s.observe(&packet(65535, false, &[0x62, 1, 0x81, 42]), t);
        s.observe(&packet(1, true, &[0x62, 1, 0x41, 44]), t);
        assert!(s.next(t).is_none());
        assert_eq!(s.missing(t), vec![0]);
        s.observe(&packet(0, false, &[0x62, 1, 1, 43]), t);
        assert_eq!(s.next(t).unwrap().unwrap().1, vec![vec![2, 1, 42, 43, 44]]);
    }
    #[test]
    fn nack_retries_are_bounded_and_large_gaps_are_compacted() {
        let t = Instant::now();
        let mut s = Source::default();
        s.observe(&packet(10, false, &[0x62, 1, 0x81, 42]), t);
        s.observe(&packet(12, true, &[0x62, 1, 0x41, 44]), t);
        for ms in [0, 20, 40] {
            assert_eq!(s.missing(t + Duration::from_millis(ms)), vec![11]);
        }
        assert!(s.missing(t + Duration::from_millis(60)).is_empty());
        let seqs = (0..1024).map(|s| s as u16).collect::<Vec<_>>();
        assert!(nack(1, 2, &seqs).len() < 300);
    }
    #[test]
    fn missing_fragment_expires_without_partial_reference_picture() {
        let t = Instant::now();
        let mut s = Source::default();
        s.observe(&packet(5, false, &[0x62, 1, 0x81, 42]), t);
        assert!(s.next(t).is_none());
        assert!(s.next(t + Duration::from_millis(76)).unwrap().is_err());
    }
    #[test]
    fn parameter_ap_without_marker_is_configuration_not_picture_loss() {
        let t = Instant::now();
        let mut s = Source::default();
        s.observe(&packet(1, false, &[0x60, 1, 0, 3, 0x40, 1, 0x80]), t);
        assert_eq!(s.next(t).unwrap().unwrap().1, vec![vec![0x40, 1, 0x80]]);
        assert!(s.next(t + Duration::from_millis(100)).is_none());
    }
    #[test]
    fn compressed_queue_byte_budget_returns_capacity_on_drop() {
        let used = Arc::new(AtomicU64::new(32 * 1024 * 1024 - 4));
        let au = access_unit(1, 1, 0, vec![vec![0; 4]], &used).unwrap();
        assert!(access_unit(1, 1, 0, vec![vec![0]], &used).is_none());
        drop(au);
        assert_eq!(used.load(Ordering::Relaxed), 32 * 1024 * 1024 - 4);
    }
    #[test]
    fn abandoned_stream_loss_does_not_invalidate_the_current_reference_chain() {
        let active = AtomicU32::new(2);
        let epoch = AtomicU64::new(0);
        let recover = AtomicBool::new(false);
        picture_lost(1, &active, &epoch, &recover);
        assert_eq!(epoch.load(Ordering::Relaxed), 0);
        assert!(!recover.load(Ordering::Relaxed));
        picture_lost(2, &active, &epoch, &recover);
        assert_eq!(epoch.load(Ordering::Relaxed), 1);
        assert!(recover.load(Ordering::Relaxed));
    }
    #[test]
    fn receiver_report_measures_loss_then_late_repair() {
        let t = Instant::now();
        let mut s = Source::default();
        s.observe(&packet(10, false, &[2, 1, 42]), t);
        s.observe(&packet(12, true, &[2, 1, 43]), t);
        let r = s.report(1, t);
        assert_eq!(&r[4..8], &[85, 0, 0, 1]);
        s.observe(&packet(11, false, &[2, 1, 44]), t);
        assert_eq!(&s.report(1, t)[4..8], &[0, 0, 0, 0]);
    }
}
