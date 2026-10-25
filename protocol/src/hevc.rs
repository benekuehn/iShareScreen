//! Validated Apple HEVC AP/FU/single-NAL depacketization. Callers supply one
//! complete, sequence-ordered RTP access unit. A malformed NAL rejects the AU;
//! partial pictures never masquerade as successfully assembled references.
use crate::Error;

pub fn aggregation_has_donl(payload: &[u8]) -> Option<bool> {
    if payload.len() < 2 || (payload[0] >> 1) & 63 != 48 {
        return None;
    }
    [true, false]
        .into_iter()
        .find(|donl| parse_aggregation(payload, *donl).is_ok())
}
fn valid_header(nalu: &[u8]) -> bool {
    nalu.len() >= 2 && nalu[0] & 128 == 0 && nalu[1] & 7 != 0 && (nalu[0] >> 1) & 63 < 48
}
fn parse_aggregation(payload: &[u8], donl: bool) -> Result<Vec<Vec<u8>>, Error> {
    let mut pos = if donl { 4 } else { 2 };
    let mut out = Vec::new();
    while pos + 2 <= payload.len() {
        let size = u16::from_be_bytes([payload[pos], payload[pos + 1]]) as usize;
        pos += 2;
        if size < 2 || pos + size > payload.len() || !valid_header(&payload[pos..pos + size]) {
            return Err(Error::InvalidPacket);
        }
        out.push(payload[pos..pos + size].to_vec());
        pos += size;
    }
    if pos != payload.len() || out.is_empty() {
        return Err(Error::InvalidPacket);
    }
    Ok(out)
}

pub fn reassemble(payloads: &[&[u8]], donl: bool) -> Result<Vec<Vec<u8>>, Error> {
    let mut out = Vec::new();
    let mut fragment: Option<Vec<u8>> = None;
    let mut bytes = 0usize;
    for payload in payloads {
        bytes = bytes
            .checked_add(payload.len())
            .ok_or(Error::InvalidPacket)?;
        if payload.len() < 2 || bytes > 16 * 1024 * 1024 || payloads.len() > 8192 {
            return Err(Error::InvalidPacket);
        }
        let kind = (payload[0] >> 1) & 63;
        match kind {
            48 => {
                if fragment.is_some() {
                    return Err(Error::InvalidPacket);
                }
                out.extend(parse_aggregation(payload, donl)?);
            }
            49 => {
                let offset = if donl { 5 } else { 3 };
                if payload.len() <= offset {
                    return Err(Error::InvalidPacket);
                }
                let header = [(payload[0] & 0x81) | ((payload[2] & 63) << 1), payload[1]];
                if !valid_header(&header) || payload[2] & 0xc0 == 0xc0 {
                    return Err(Error::InvalidPacket);
                }
                if payload[2] & 128 != 0 {
                    if fragment.is_some() {
                        return Err(Error::InvalidPacket);
                    }
                    fragment = Some(header.to_vec());
                }
                let buf = fragment.as_mut().ok_or(Error::InvalidPacket)?;
                if buf[..2] != header {
                    return Err(Error::InvalidPacket);
                }
                buf.extend_from_slice(&payload[offset..]);
                if payload[2] & 64 != 0 {
                    out.push(fragment.take().unwrap());
                }
            }
            _ => {
                if fragment.is_some() {
                    return Err(Error::InvalidPacket);
                }
                let nalu = if donl {
                    if payload.len() < 4 {
                        return Err(Error::InvalidPacket);
                    }
                    [payload[..2].to_vec(), payload[4..].to_vec()].concat()
                } else {
                    payload.to_vec()
                };
                if !valid_header(&nalu) {
                    return Err(Error::InvalidPacket);
                }
                out.push(nalu);
            }
        }
    }
    if fragment.is_some() {
        return Err(Error::InvalidPacket);
    }
    Ok(out)
}

/// Startup/resize can carry a parameter AP alongside an unfinished picture.
/// Harvest only independently validated parameter NALs from that prefix; never
/// forward the unfinished picture to the decoder as a complete access unit.
pub fn harvest_parameters(payloads: &[&[u8]], donl: bool) -> Result<Vec<Vec<u8>>, Error> {
    if payloads.len() > 8192
        || payloads.iter().map(|payload| payload.len()).sum::<usize>() > 16 * 1024 * 1024
    {
        return Err(Error::InvalidPacket);
    }
    if let Ok(nalus) = reassemble(payloads, donl) {
        return Ok(nalus
            .into_iter()
            .filter(|nalu| (32..=34).contains(&((nalu[0] >> 1) & 63)))
            .collect());
    }
    let mut out = Vec::new();
    for payload in payloads {
        if payload.len() < 2 {
            continue;
        }
        let kind = (payload[0] >> 1) & 63;
        let nalus = if kind == 48 {
            parse_aggregation(payload, donl)
        } else if (32..=34).contains(&kind) {
            reassemble(&[payload], donl)
        } else {
            continue;
        };
        if let Ok(nalus) = nalus {
            out.extend(
                nalus
                    .into_iter()
                    .filter(|nalu| (32..=34).contains(&((nalu[0] >> 1) & 63))),
            );
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parameter_prefix_is_harvested_without_publishing_an_unfinished_picture() {
        let parameters = &[0x60, 1, 0, 3, 0x40, 1, 0x80][..];
        let partial_picture = &[0x62, 1, 0x81, 0x80, 42][..];
        assert!(reassemble(&[parameters, partial_picture], false).is_err());
        assert_eq!(
            harvest_parameters(&[parameters, partial_picture], false).unwrap(),
            vec![vec![0x40, 1, 0x80]]
        );
    }
    #[test]
    fn fragment_is_never_published_without_both_boundaries() {
        let first = &[0x62, 1, 0x81, 0x80, 42][..];
        let last = &[0x62, 1, 0x41, 43, 44][..];
        assert_eq!(
            reassemble(&[first, last], false).unwrap(),
            vec![vec![2, 1, 0x80, 42, 43, 44]]
        );
        assert_eq!(reassemble(&[first], false), Err(Error::InvalidPacket));
        assert_eq!(reassemble(&[last], false), Err(Error::InvalidPacket));
    }
    #[test]
    fn malformed_ap_rejects_the_entire_au_instead_of_feeding_its_prefix() {
        let valid = [0x60, 1, 0, 3, 0x40, 1, 0x80];
        assert_eq!(aggregation_has_donl(&valid), Some(false));
        let mut partial = valid.to_vec();
        partial.extend_from_slice(&[0, 20, 0x42, 1]);
        assert_eq!(reassemble(&[&partial], false), Err(Error::InvalidPacket));
    }
    #[test]
    fn apple_donl_is_removed_from_every_fu_fragment() {
        let first = &[0x62, 1, 0x81, 0, 9, 0x80, 42][..];
        let last = &[0x62, 1, 0x41, 0, 9, 43, 44][..];
        assert_eq!(
            reassemble(&[first, last], true).unwrap(),
            vec![vec![2, 1, 0x80, 42, 43, 44]]
        );
    }
}
