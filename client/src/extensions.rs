//! Bounded cursor cache and multi-record UTF-8 pasteboard synchronization.
use flate2::{Compression, Decompress, FlushDecompress, write::ZlibEncoder};
use std::{collections::BTreeMap, io::Write};
#[derive(Clone)]
pub struct Cursor {
    pub width: u16,
    pub height: u16,
    pub x: u16,
    pub y: u16,
    pub rgba: Vec<u8>,
}
#[derive(Default)]
pub struct Updates {
    pub cursor: Option<Option<Cursor>>,
    pub clipboard: Option<String>,
    pub canvas: Option<(u32, u32)>,
}
#[derive(Default)]
pub struct Extensions {
    cache: BTreeMap<u32, Cursor>,
    clipboard: Vec<u8>,
    expected: usize,
}
fn u16be(data: &[u8], pos: usize) -> Option<u16> {
    Some(u16::from_be_bytes(data.get(pos..pos + 2)?.try_into().ok()?))
}
fn u32be(data: &[u8], pos: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(pos..pos + 4)?.try_into().ok()?))
}
fn inflate(data: &[u8], limit: usize) -> Option<Vec<u8>> {
    let mut out = vec![0; limit + 1];
    let mut z = Decompress::new(true);
    z.decompress(data, &mut out, FlushDecompress::Sync).ok()?;
    if z.total_in() != data.len() as u64 || z.total_out() > limit as u64 {
        return None;
    }
    out.truncate(z.total_out() as usize);
    Some(out)
}
pub fn clipboard_send(text: &str) -> Option<Vec<u8>> {
    if text.len() > 1024 * 1024 {
        return None;
    }
    let uti = b"public.utf8-plain-text";
    let mut inner = Vec::new();
    inner.extend(1u32.to_be_bytes());
    inner.extend((uti.len() as u32).to_be_bytes());
    inner.extend(uti);
    inner.extend([0; 8]);
    inner.extend((text.len() as u32).to_be_bytes());
    inner.extend(text.as_bytes());
    let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
    z.write_all(&inner).ok()?;
    let compressed = z.finish().ok()?;
    let mut out = vec![0x1f, 0, 0, 0, 0, 0, 0, 0];
    out.extend((inner.len() as u32).to_be_bytes());
    out.extend((compressed.len() as u32).to_be_bytes());
    out.extend(compressed);
    Some(out)
}
fn clipboard_text(inner: &[u8]) -> Option<String> {
    fn number(d: &[u8], p: &mut usize) -> Option<u32> {
        let n = u32be(d, *p)?;
        *p += 4;
        Some(n)
    }
    fn bytes<'a>(d: &'a [u8], p: &mut usize) -> Option<&'a [u8]> {
        let n = number(d, p)? as usize;
        let b = d.get(*p..p.checked_add(n)?)?;
        *p += n;
        Some(b)
    }
    let mut p = 0;
    let count = number(inner, &mut p)?;
    if count > 256 {
        return None;
    }
    for _ in 0..count {
        let uti = bytes(inner, &mut p)?;
        number(inner, &mut p)?;
        let aliases = number(inner, &mut p)?;
        if aliases > 256 {
            return None;
        }
        for _ in 0..aliases {
            bytes(inner, &mut p)?;
            bytes(inner, &mut p)?;
        }
        let text = bytes(inner, &mut p)?;
        if uti == b"public.utf8-plain-text" {
            return String::from_utf8(text.to_vec()).ok();
        }
    }
    None
}
impl Extensions {
    pub fn consume(&mut self, message: &[u8], updates: &mut Updates) -> Option<Vec<u8>> {
        if self.expected > 0 || message.first() == Some(&0x1f) {
            if self.expected == 0 {
                if message.len() < 16 {
                    return None;
                }
                let size = u32be(message, 12)? as usize;
                let expanded = u32be(message, 8)? as usize;
                if size > 1024 * 1024 || expanded > 1024 * 1024 {
                    return None;
                }
                self.expected = 16 + size;
            }
            if self.clipboard.len() + message.len() > self.expected {
                self.clipboard.clear();
                self.expected = 0;
                return None;
            }
            self.clipboard.extend(message);
            if self.clipboard.len() == self.expected {
                if self.clipboard[2] == 0 {
                    if let Some(inner) = inflate(&self.clipboard[16..], 1024 * 1024) {
                        updates.clipboard = clipboard_text(&inner);
                    }
                }
                self.clipboard.clear();
                self.expected = 0;
            }
            return None;
        }
        if message.len() >= 8 && message[0] == 0x14 && u16be(message, 6) == Some(2) {
            return Some(vec![0x0b, 0, 0, 0, 0, 0, 0, 0]);
        }
        if message.first() != Some(&0) {
            return None;
        }
        let count = u16be(message, 2)?;
        if count > 128 {
            return None;
        }
        let mut p = 4;
        for _ in 0..count {
            let x = u16be(message, p)?;
            let y = u16be(message, p + 2)?;
            let w = u16be(message, p + 4)?;
            let h = u16be(message, p + 6)?;
            let kind = u32be(message, p + 8)?;
            p += 12;
            if kind == 1104 {
                let id = u32be(message, p)?;
                let size = u32be(message, p + 4)? as usize;
                p += 8;
                let data = message.get(p..p.checked_add(size)?)?;
                p += size;
                if size == 0 {
                    updates.cursor = Some(self.cache.get(&id).cloned());
                    continue;
                }
                if w == 0 || h == 0 || w > 256 || h > 256 || x >= w || y >= h {
                    continue;
                }
                let pixels = w as usize * h as usize;
                let Some(raw) = inflate(data, pixels * 5) else {
                    continue;
                };
                if raw.len() != pixels * 5 {
                    continue;
                }
                let mut rgba = vec![0; pixels * 4];
                for i in 0..pixels {
                    rgba[i * 4..i * 4 + 4].copy_from_slice(&[
                        raw[i * 4 + 2],
                        raw[i * 4 + 1],
                        raw[i * 4],
                        raw[pixels * 4 + i],
                    ]);
                }
                let cursor = Cursor {
                    width: w,
                    height: h,
                    x,
                    y,
                    rgba,
                };
                if self.cache.len() >= 64 {
                    self.cache.pop_first();
                }
                self.cache.insert(id, cursor.clone());
                updates.cursor = Some(Some(cursor));
            } else if [1010, 1011, 1105, 1107, 1109, 1110, 1002].contains(&kind) {
                let size = u16be(message, p)? as usize;
                p += 2;
                let data = message.get(p..p.checked_add(size)?)?;
                if kind == 1105 && size >= 10 {
                    let w = u16be(data, 6)? as u32;
                    let h = u16be(data, 8)? as u32;
                    if w > 0 && h > 0 && w <= 16384 && h <= 16384 {
                        updates.canvas = Some((w, h));
                    }
                }
                p += size;
            } else {
                return None;
            }
        }
        None
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clipboard_roundtrip_across_cipher_record_boundaries() {
        let message = clipboard_send("synthetic UTF-8 🦀").unwrap();
        let mut ext = Extensions::default();
        let mut updates = Updates::default();
        ext.consume(&message[..19], &mut updates);
        assert!(updates.clipboard.is_none());
        ext.consume(&message[19..], &mut updates);
        assert_eq!(updates.clipboard.as_deref(), Some("synthetic UTF-8 🦀"));
    }
    #[test]
    fn malformed_or_oversized_pasteboard_never_allocates_declared_length() {
        let mut ext = Extensions::default();
        let mut updates = Updates::default();
        let mut bad = vec![0x1f; 16];
        bad[12..].copy_from_slice(&u32::MAX.to_be_bytes());
        ext.consume(&bad, &mut updates);
        assert_eq!(ext.expected, 0);
        assert!(ext.clipboard.is_empty());
    }
    #[test]
    fn cursor_bgra_mask_and_cached_select() {
        let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
        z.write_all(&[10, 20, 30, 255, 128]).unwrap();
        let compressed = z.finish().unwrap();
        let mut msg = vec![0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 4, 80];
        msg.extend(7u32.to_be_bytes());
        msg.extend((compressed.len() as u32).to_be_bytes());
        msg.extend(compressed);
        let mut ext = Extensions::default();
        let mut updates = Updates::default();
        ext.consume(&msg, &mut updates);
        assert_eq!(
            updates.cursor.take().unwrap().unwrap().rgba,
            vec![30, 20, 10, 128]
        );
        msg.truncate(24);
        msg[20..24].fill(0);
        ext.consume(&msg, &mut updates);
        assert!(updates.cursor.unwrap().is_some());
    }
}
