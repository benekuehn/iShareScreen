//! RFB and AVConference media configuration, independent of the viewer UI.
use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use isharescreen_protocol::control::Cipher;
use plist::{Dictionary, Value};
use std::io::{Cursor, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use zeroize::Zeroizing;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const ENCODINGS: [i32; 13] = [
    1010, 1011, 1002, 6, 16, 1104, 1100, -223, 1101, 1105, 1107, 1109, 1110,
];

pub fn encodings() -> Vec<u8> {
    let mut out = vec![2, 0, 0, ENCODINGS.len() as u8];
    for encoding in ENCODINGS {
        out.extend_from_slice(&encoding.to_be_bytes());
    }
    out
}
pub fn viewer_info() -> Vec<u8> {
    let mut out = vec![0x21, 0, 0, 0x3e, 0, 1];
    for value in [2u32, 6, 1, 0, 15, 3, 0] {
        out.extend_from_slice(&value.to_be_bytes());
    }
    let mut mask = [0u8; 32];
    for (index, value) in [(0, 0xb0), (2, 0x0c), (3, 3), (4, 0x90), (10, 0x40)] {
        mask[index] = value;
    }
    out.extend(mask);
    out.extend_from_slice(&[0x12, 0, 0, 1, 0, 1, 0, 1, 0, 0, 0, 1]);
    out
}
fn u16_at(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
}
fn u32_at(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}

pub fn virtual_display(width: u32, height: u32, scale: f64) -> Result<Vec<u8>> {
    if width < 2
        || height < 2
        || !scale.is_finite()
        || !(1.0..=4.0).contains(&scale)
        || width as f64 * scale > 3840.0
        || height as f64 * scale > 2160.0
    {
        return Err("display request is outside supported 3840x2160 backing".into());
    }
    let mut info = vec![0u8; 0x9c + 28 * 5];
    let size = info.len() as u16;
    u16_at(&mut info, 0, size);
    let name = b"iShareScreen Virtual Display";
    info[2..2 + name.len()].copy_from_slice(name);
    u32_at(&mut info, 0x7a, 1);
    u32_at(&mut info, 0x7e, 4);
    info[0x82..0x86].copy_from_slice(&369.4545593261719f32.to_be_bytes());
    info[0x86..0x8a].copy_from_slice(&207.81817626953125f32.to_be_bytes());
    u32_at(&mut info, 0x8a, 3840);
    u32_at(&mut info, 0x8e, 2160);
    u32_at(&mut info, 0x96, 7);
    u16_at(&mut info, 0x9a, 5);
    for (i, (w, h)) in [
        (1920., 1080.),
        (1440., 900.),
        (1920., 1080.),
        (1440., 810.),
        (1312., 848.),
    ]
    .into_iter()
    .enumerate()
    {
        let points_w = (w * width as f64 / 1920. + 0.5) as u32;
        let points_h = (h * height as f64 / 1080. + 0.5) as u32;
        let offset = 0x9c + 28 * i;
        for (n, value) in [
            (points_w as f64 * scale + 0.5) as u32,
            (points_h as f64 * scale + 0.5) as u32,
            points_w,
            points_h,
        ]
        .into_iter()
        .enumerate()
        {
            u32_at(&mut info, offset + n * 4, value);
        }
        info[offset + 16..offset + 24].copy_from_slice(&60f64.to_be_bytes());
    }
    let mut out = vec![0x1d, 0];
    out.extend_from_slice(&(8 + size).to_be_bytes());
    out.extend_from_slice(&[0, 1, 0, 1, 0, 0, 0, 0]);
    out.extend(info);
    Ok(out)
}
pub fn fbu(full: bool) -> Vec<u8> {
    vec![
        3,
        if full { 0 } else { 1 },
        0,
        0,
        0,
        0,
        if full { 255 } else { 0 },
        if full { 255 } else { 1 },
        if full { 255 } else { 0 },
        if full { 255 } else { 1 },
    ]
}
fn varint(mut value: u64, out: &mut Vec<u8>) {
    while value > 127 {
        out.push((value as u8 & 127) | 128);
        value >>= 7;
    }
    out.push(value as u8);
}
fn number(out: &mut Vec<u8>, field: u64, value: u64) {
    varint(field << 3, out);
    varint(value, out);
}
fn bytes(out: &mut Vec<u8>, field: u64, value: &[u8]) {
    varint((field << 3) | 2, out);
    varint(value.len() as u64, out);
    out.extend_from_slice(value);
}
fn random<const N: usize>() -> Result<[u8; N]> {
    let mut out = [0u8; N];
    openssl::rand::rand_bytes(&mut out)?;
    Ok(out)
}
fn uuid() -> Result<(String, [u8; 16])> {
    let mut value = random::<16>()?;
    value[6] = (value[6] & 15) | 64;
    value[8] = (value[8] & 63) | 128;
    let hex: String = value.iter().map(|byte| format!("{byte:02X}")).collect();
    Ok((
        format!(
            "{}-{}-{}-{}-{}",
            &hex[..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..]
        ),
        value,
    ))
}
fn media_blob(mode: u64, ssrc: u32, timestamp: u64, audio: bool) -> Vec<u8> {
    let mut desc = Vec::new();
    number(&mut desc, 1, ssrc as u64);
    if mode == 7 {
        number(&mut desc, 2, 1);
        let mut bank = Vec::new();
        // Apple's mapping is inverted: the bank labelled 100 yields HEVC.
        number(&mut bank, 1, 100);
        for alternate in [1, 2] {
            let mut entry = Vec::new();
            for (field, value) in [(1, 1), (2, alternate), (3, 50115), (4, 0)] {
                number(&mut entry, field, value);
            }
            bytes(&mut bank, 2, &entry);
        }
        bytes(
            &mut bank,
            3,
            b"FLS;LF:-1;POS:5;EOD:1;HTS:2;RR:3;POSE:4;AR:16/9,5/8;XR:16/9,5/8;",
        );
        number(&mut bank, 4, 14);
        bytes(&mut desc, 3, &bank);
        for (field, value) in [(6, 1), (7, 0), (8, 63), (9, 1), (12, 1)] {
            number(&mut desc, field, value);
        }
    } else {
        for (field, value) in [
            (2, 0),
            (3, 0),
            (4, if audio { 24191 } else { 1000 }),
            (5, 0),
            (6, 0),
        ] {
            number(&mut desc, field, value);
        }
    }
    let mut out = Vec::new();
    number(&mut out, 1, 1);
    number(&mut out, 2, 1);
    bytes(&mut out, if mode == 7 { 5 } else { 3 }, &desc);
    bytes(&mut out, 6, b"iShareScreen 0.1.0");
    number(&mut out, 8, 0);
    for (kind, bitrate, buffer) in [
        (0, 40_000_000, Some(12288)),
        (0, 6_000_000, Some(131072)),
        (4074, 0, Some(16384)),
        (16, 4100, None),
        (0, 75_000_000, Some(524288)),
        (0, 20_000_000, Some(98304)),
        (4, 6500, None),
        (0, 60_000_000, Some(262144)),
        (1, 299, None),
        (0, 100_000_000, Some(1048576)),
    ] {
        let mut tier = Vec::new();
        number(&mut tier, 1, kind);
        number(&mut tier, 2, bitrate);
        if let Some(buffer) = buffer {
            number(&mut tier, 3, buffer);
        }
        bytes(&mut out, 9, &tier);
    }
    for (field, value) in [(13, timestamp), (14, 2), (16, 0), (18, 1)] {
        number(&mut out, field, value);
    }
    out
}
fn offer(mode: u64, audio: bool) -> Result<(Vec<u8>, u32)> {
    let ssrc = u32::from_be_bytes(random::<4>()?);
    let blob = media_blob(
        mode,
        ssrc,
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64,
        audio,
    );
    let mut compressed = ZlibEncoder::new(Vec::new(), Compression::default());
    compressed.write_all(&blob)?;
    let mut endpoint = Vec::new();
    number(&mut endpoint, 1, 0);
    number(&mut endpoint, 2, 1);
    bytes(&mut endpoint, 3, b"Linux-x86_64");
    bytes(&mut endpoint, 4, b"1.0.0");
    bytes(&mut endpoint, 5, b"0");
    let mut dict = Dictionary::new();
    dict.insert(
        "avcMediaStreamOptionRemoteEndpointInfo".into(),
        Value::Data(endpoint),
    );
    dict.insert(
        "avcMediaStreamNegotiatorMode".into(),
        Value::Integer(mode.into()),
    );
    dict.insert(
        "avcMediaStreamNegotiatorMediaBlob".into(),
        Value::Data(compressed.finish()?),
    );
    dict.insert(
        "avcMediaStreamOptionCallID".into(),
        Value::String(uuid()?.0),
    );
    let mut out = Vec::new();
    Value::Dictionary(dict).to_writer_binary(&mut out)?;
    Ok((out, ssrc))
}

pub struct MediaKeys {
    pub audio_send: Zeroizing<[u8; 46]>,
    pub audio_receive: Zeroizing<[u8; 46]>,
    pub video_send: Zeroizing<[u8; 46]>,
    pub video_receive: Zeroizing<[u8; 46]>,
}
impl MediaKeys {
    fn new() -> Result<Self> {
        Ok(Self {
            audio_send: Zeroizing::new(random()?),
            audio_receive: Zeroizing::new(random()?),
            video_send: Zeroizing::new(random()?),
            video_receive: Zeroizing::new(random()?),
        })
    }
}
fn media_config(audio: &[u8], video: &[u8], keys: &MediaKeys) -> Result<Vec<u8>> {
    let size = audio.len() + video.len() + 0xd8;
    if size > 65535 {
        return Err("media offer exceeds record limit".into());
    }
    let mut out = vec![0u8; size + 4];
    out[0] = 0x1c;
    u16_at(&mut out, 2, size as u16);
    u16_at(&mut out, 4, 3);
    u32_at(&mut out, 6, 5); // one 60 Hz stream, native cursor overlay
    u16_at(&mut out, 10, audio.len() as u16);
    u16_at(&mut out, 12, video.len() as u16);
    out[0x14..0x24].copy_from_slice(&uuid()?.1);
    out[0x24..0x52].copy_from_slice(&*keys.audio_send);
    out[0x52..0x80].copy_from_slice(&*keys.audio_receive);
    out[0x80..0x80 + audio.len()].copy_from_slice(audio);
    let offset = 0x80 + audio.len();
    out[offset..offset + 46].copy_from_slice(&*keys.video_send);
    out[offset + 46..offset + 92].copy_from_slice(&*keys.video_receive);
    out[offset + 92..offset + 92 + video.len()].copy_from_slice(video);
    Ok(out)
}

fn read_varint(data: &[u8], pos: &mut usize) -> Result<u64> {
    let mut value = 0u64;
    for shift in (0..70).step_by(7) {
        let byte = *data.get(*pos).ok_or("truncated protobuf varint")?;
        *pos += 1;
        if shift == 63 && byte > 1 {
            return Err("protobuf integer overflow".into());
        }
        value |= ((byte & 127) as u64) << shift;
        if byte & 128 == 0 {
            return Ok(value);
        }
    }
    Err("unterminated protobuf varint".into())
}
fn fields(data: &[u8]) -> Result<Vec<(u64, Option<u64>, &[u8])>> {
    let mut pos = 0;
    let mut out = Vec::new();
    while pos < data.len() {
        let tag = read_varint(data, &mut pos)?;
        match tag & 7 {
            0 => out.push((tag >> 3, Some(read_varint(data, &mut pos)?), &[][..])),
            1 | 2 | 5 => {
                let length = match tag & 7 {
                    1 => 8,
                    5 => 4,
                    _ => usize::try_from(read_varint(data, &mut pos)?)?,
                };
                let end = pos.checked_add(length).ok_or("protobuf length overflow")?;
                let value = data.get(pos..end).ok_or("truncated protobuf field")?;
                out.push((tag >> 3, None, value));
                pos = end;
            }
            _ => return Err("unsupported protobuf wire type".into()),
        }
    }
    Ok(out)
}
fn plist_dims(value: &Value) -> Result<(u32, u32, u32)> {
    let blob = value
        .as_dictionary()
        .and_then(|dict| dict.get("avcMediaStreamNegotiatorMediaBlob"))
        .and_then(Value::as_data)
        .ok_or("missing media blob")?;
    let mut decoded = Vec::new();
    ZlibDecoder::new(blob)
        .take(131073)
        .read_to_end(&mut decoded)?;
    if decoded.len() > 131072 {
        return Err("media answer decompression limit exceeded".into());
    }
    for (field, _, data) in fields(&decoded)? {
        if field != 5 {
            continue;
        }
        let mut dims = [0; 3];
        for (field, value, _) in fields(data)? {
            if (4..=6).contains(&field) {
                dims[field as usize - 4] = u32::try_from(value.ok_or("invalid canvas value")?)?;
            }
        }
        if dims[0] > 0 && dims[1] > 0 && dims[0] <= 16384 && dims[1] <= 16384 && dims[2] <= 64 {
            return Ok((dims[0], dims[1], dims[2]));
        }
    }
    Err("canvas not ready".into())
}
pub fn answer_dims(message: &[u8]) -> Option<(u32, u32, u32)> {
    if message.len() > 65535 {
        return None;
    }
    // Find exact binary-plist trailers without asking the parser to accept
    // trailing keys/other plists. Candidate counts and inflation are bounded.
    let positions: Vec<_> = message
        .windows(8)
        .enumerate()
        .filter(|(_, bytes)| *bytes == b"bplist00")
        .take(8)
        .map(|(pos, _)| pos)
        .collect();
    for start in positions {
        for end in start + 40..=message.len() {
            let trailer = &message[end - 32..end];
            if trailer[..6] != [0; 6]
                || ![1, 2, 4, 8].contains(&trailer[6])
                || ![1, 2, 4, 8].contains(&trailer[7])
            {
                continue;
            }
            let count = u64::from_be_bytes(trailer[8..16].try_into().ok()?);
            let top = u64::from_be_bytes(trailer[16..24].try_into().ok()?);
            let offset = u64::from_be_bytes(trailer[24..32].try_into().ok()?);
            if count == 0 || count > 65535 || top >= count || offset < 8 {
                continue;
            }
            if offset.checked_add(count * trailer[6] as u64) != Some((end - start - 32) as u64) {
                continue;
            }
            if let Ok(value) = Value::from_reader(Cursor::new(&message[start..end])) {
                if let Ok(dims) = plist_dims(&value) {
                    return Some(dims);
                }
            }
        }
    }
    None
}

pub struct Link {
    pub socket: TcpStream,
    cipher: Cipher,
    buffer: Vec<u8>,
}
fn pointer_message(key: &[u8], buttons: u8, x: u16, y: u16) -> Result<Vec<u8>> {
    let mut plain = [0u8; 16];
    plain[10] = 255;
    plain[11] = buttons;
    plain[12..14].copy_from_slice(&x.to_be_bytes());
    plain[14..16].copy_from_slice(&y.to_be_bytes());
    let mut cryptor = openssl::symm::Crypter::new(
        openssl::symm::Cipher::aes_128_ecb(),
        openssl::symm::Mode::Encrypt,
        key,
        None,
    )?;
    cryptor.pad(false);
    let mut encrypted = [0u8; 32];
    let size = cryptor.update(&plain, &mut encrypted)?;
    cryptor.finalize(&mut encrypted[size..])?;
    let mut message = vec![0x10, 0];
    message.extend_from_slice(&encrypted[..16]);
    Ok(message)
}
impl Link {
    pub fn pointer(&mut self, buttons: u8, x: u16, y: u16) -> Result<()> {
        let message = pointer_message(self.cipher.cbc_key(), buttons, x, y)?;
        self.send(&message)
    }
    pub fn send(&mut self, message: &[u8]) -> Result<()> {
        self.socket.write_all(&self.cipher.encrypt(message)?)?;
        Ok(())
    }
    pub fn poll(&mut self) -> Result<Vec<Vec<u8>>> {
        let mut buffer = [0u8; 65536];
        match self.socket.read(&mut buffer) {
            Ok(0) => return Err("server closed the control connection".into()),
            Ok(size) => self.buffer.extend_from_slice(&buffer[..size]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error.into()),
        }
        if self.buffer.len() > 1024 * 1024 {
            return Err("control receive buffer limit exceeded".into());
        }
        let mut used = 0;
        let mut messages = Vec::new();
        while used + 2 <= self.buffer.len() {
            let length = u16::from_be_bytes(self.buffer[used..used + 2].try_into()?) as usize;
            if length == 0 || length % 16 != 0 {
                return Err("invalid encrypted control record length".into());
            }
            if used + 2 + length > self.buffer.len() {
                break;
            }
            if let Ok(message) = self
                .cipher
                .decrypt(&self.buffer[used + 2..used + 2 + length])
            {
                messages.push(message);
            }
            used += 2 + length;
        }
        self.buffer.drain(..used);
        Ok(messages)
    }
}

pub struct Negotiated {
    pub link: Link,
    pub keys: MediaKeys,
    pub video_ssrc: u32,
    pub audio_ssrc: u32,
    pub width: u32,
    pub height: u32,
    pub pending: Vec<Vec<u8>>,
}

fn exact(stream: &mut TcpStream, count: usize) -> Result<Vec<u8>> {
    let mut bytes = vec![0; count];
    stream.read_exact(&mut bytes)?;
    Ok(bytes)
}
pub fn negotiate(
    mut socket: TcpStream,
    key: &[u8],
    width: u32,
    height: u32,
    scale: f64,
    curtain: bool,
    audio: bool,
) -> Result<Negotiated> {
    socket.write_all(&[0xc1])?;
    let init = exact(&mut socket, 24)?;
    let name_len = u32::from_be_bytes(init[20..24].try_into()?) as usize;
    if name_len > 65535 {
        return Err("server name exceeds limit".into());
    }
    exact(&mut socket, name_len)?;
    socket.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut peek = [0u8; 4];
    if socket
        .peek(&mut peek)
        .is_ok_and(|count| count == 4 && peek == [0, 0x4a, 0, 1])
    {
        let prompt = exact(&mut socket, 76)?;
        let console = prompt[12..].split(|byte| *byte == 0).next().unwrap_or(&[]);
        let mut body = vec![0u8; 72];
        body[1] = 2;
        let size = console.len().min(63);
        body[8..8 + size].copy_from_slice(&console[..size]);
        socket.write_all(&72u16.to_be_bytes())?;
        socket.write_all(&body)?;
        socket.set_read_timeout(Some(Duration::from_secs(60)))?;
        let mut ack = [0u8; 65536];
        if socket.read(&mut ack)? == 0 {
            return Err("session selection was rejected".into());
        }
    }
    socket.set_read_timeout(Some(Duration::from_secs(15)))?;
    socket.write_all(&viewer_info())?;
    std::thread::sleep(Duration::from_millis(100));
    if curtain {
        socket.write_all(&virtual_display(width, height, scale)?)?;
    }
    socket.write_all(&encodings())?;
    let mut cipher = None;
    for _ in 0..64 {
        let message = exact(&mut socket, 1)?[0];
        if message == 0x14 {
            exact(&mut socket, 7)?;
            continue;
        }
        if message == 2 {
            continue;
        }
        if message != 0 {
            return Err("unexpected plaintext handshake message".into());
        }
        let update = exact(&mut socket, 3)?;
        let count = u16::from_be_bytes([update[1], update[2]]);
        if count > 128 {
            return Err("too many handshake rectangles".into());
        }
        for _ in 0..count {
            let rect = exact(&mut socket, 12)?;
            match i32::from_be_bytes(rect[8..12].try_into()?) {
                1103 => cipher = Some(Cipher::new(&exact(&mut socket, 36)?, key)?),
                1010 | 1011 => {
                    let size = exact(&mut socket, 2)?;
                    exact(&mut socket, u16::from_be_bytes([size[0], size[1]]) as usize)?;
                }
                _ => return Err("unsupported plaintext handshake rectangle".into()),
            }
        }
        if cipher.is_some() {
            break;
        }
    }
    let mut link = Link {
        socket,
        cipher: cipher.ok_or("server did not advertise enc1103")?,
        buffer: Vec::new(),
    };
    link.socket.write_all(&[0x12, 0, 0, 2, 0, 1, 0, 0])?;
    std::thread::sleep(Duration::from_millis(200));
    link.socket
        .set_read_timeout(Some(Duration::from_millis(100)))?;
    let mut pending = Vec::new();
    for _ in 0..5 {
        pending.extend(link.poll()?);
    }
    let (video, video_ssrc) = offer(7, audio)?;
    let (audio_offer, audio_ssrc) = offer(8, audio)?;
    let keys = MediaKeys::new()?;
    let configuration = Zeroizing::new(media_config(&audio_offer, &video, &keys)?);
    link.send(&encodings())?;
    link.send(&configuration)?;
    link.send(&fbu(true))?;
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut retry = Instant::now() + Duration::from_millis(250);
    let mut attempts = 0;
    while Instant::now() < deadline {
        let received = link.poll()?;
        let mut received = received.into_iter();
        while let Some(message) = received.next() {
            if let Some((width, height, _tiles)) = answer_dims(&message) {
                pending.extend(received);
                return Ok(Negotiated {
                    link,
                    keys,
                    video_ssrc,
                    audio_ssrc,
                    width,
                    height,
                    pending,
                });
            }
            pending.push(message);
            if pending.len() > 256 {
                return Err("negotiation event buffer limit exceeded".into());
            }
        }
        if Instant::now() >= retry && attempts < 16 {
            link.send(&configuration)?;
            attempts += 1;
            retry = Instant::now() + Duration::from_millis(250);
        }
    }
    Err("server did not negotiate a video canvas".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn unhex(value: &serde_json::Value) -> Vec<u8> {
        let value = value.as_str().unwrap();
        (0..value.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&value[i..i + 2], 16).unwrap())
            .collect()
    }
    #[test]
    fn rfb_and_media_builders_match_independent_reference() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/wire-vector.json")).unwrap();
        assert_eq!(fixture["synthetic_only"], true);
        assert_eq!(viewer_info(), unhex(&fixture["viewer_info"]));
        assert_eq!(encodings(), unhex(&fixture["encodings"]));
        assert_eq!(
            pointer_message(&(0u8..16).collect::<Vec<_>>(), 3, 1234, 567).unwrap(),
            unhex(&fixture["pointer"])
        );
        assert_eq!(
            virtual_display(1920, 1080, 2.).unwrap(),
            unhex(&fixture["virtual_display"])
        );
        assert_eq!(
            media_blob(7, 0x12345678, 1234567890, false),
            unhex(&fixture["video_blob"])
        );
        assert_eq!(
            media_blob(8, 0x12345678, 1234567890, false),
            unhex(&fixture["audio_blob"])
        );
        assert_eq!(
            answer_dims(&unhex(&fixture["answer"])),
            Some((3840, 2160, 1))
        );
    }
    #[test]
    fn malformed_lengths_and_oversized_geometry_are_rejected() {
        assert!(
            fields(&[
                0x12, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff
            ])
            .is_err()
        );
        assert!(fields(&[0x13]).is_err());
        assert!(virtual_display(3840, 2160, 2.).is_err());
        assert!(virtual_display(1920, 1080, f64::NAN).is_err());
        for size in 0..128 {
            assert_eq!(answer_dims(&vec![0; size]), None);
        }
    }
    #[test]
    fn incomplete_tcp_records_survive_read_boundaries() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut sender = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        sender.set_nodelay(true).unwrap();
        let (receiver, _) = listener.accept().unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let blob = [7; 36];
        let key = [9; 16];
        let mut cipher = Cipher::new(&blob, &key).unwrap();
        let mut link = Link {
            socket: receiver,
            cipher: Cipher::new(&blob, &key).unwrap(),
            buffer: Vec::new(),
        };
        let record = cipher.encrypt(b"synthetic record").unwrap();
        sender.write_all(&record[..1]).unwrap();
        assert!(link.poll().unwrap().is_empty());
        sender.write_all(&record[1..]).unwrap();
        let mut messages = Vec::new();
        for _ in 0..5 {
            messages.extend(link.poll().unwrap());
            if !messages.is_empty() {
                break;
            }
        }
        assert_eq!(messages, vec![b"synthetic record".to_vec()]);
    }
}
