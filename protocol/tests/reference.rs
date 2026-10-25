//! Cross-language, synthetic known-answer vectors. Not self-roundtrip tests.
use isharescreen_protocol::{control, srtp, Error};
use serde_json::Value;

fn hex(value: &Value) -> Vec<u8> {
    let text = value.as_str().unwrap();
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i+2], 16).unwrap()).collect()
}
fn vectors() -> Value { serde_json::from_str(include_str!("vectors.json")).unwrap() }

#[test]
fn decrypts_reference_rtp_across_rollover_retransmit_and_extensions() {
    let data = vectors();
    assert_eq!(data["synthetic_only"], true);
    let mut receiver = srtp::Receiver::new(&hex(&data["blob"])).unwrap();
    for vector in data["rtp"].as_array().unwrap() {
        let decoded = receiver.unprotect(&hex(&vector["packet"])).unwrap();
        assert_eq!(decoded.header, hex(&vector["header"]));
        assert_eq!(decoded.payload, hex(&vector["payload"]));
    }
    assert_eq!(receiver.counts()[&0x12345678], 5);
}

#[test]
fn srtcp_matches_reference_in_both_directions_and_at_last_index() {
    let data = vectors();
    let mut control = srtp::Control::new(&hex(&data["blob"])).unwrap();
    for (i, vector) in data["rtcp"].as_array().unwrap().iter().enumerate() {
        let plain = hex(&vector["plain"]);
        let packet = hex(&vector["packet"]);
        assert_eq!(control.unprotect(&packet).unwrap(), plain);
        if i < 2 { assert_eq!(control.protect(&plain).unwrap(), packet); }
        let mut tampered = packet;
        tampered[4] ^= 1;
        assert_eq!(control.unprotect(&tampered), Err(Error::Authentication));
    }
}

#[test]
fn control_matches_reference_with_continuous_cbc_counter_gap_and_bad_digest() {
    let data = vectors();
    let mut receiver = control::Cipher::new(&hex(&data["control_blob"]), &hex(&data["auth_key"])).unwrap();
    let mut sender = control::Cipher::new(&hex(&data["control_blob"]), &hex(&data["auth_key"])).unwrap();
    for (i, vector) in data["records"].as_array().unwrap().iter().enumerate() {
        let packet = hex(&vector["packet"]);
        if vector["plain"].is_null() {
            assert_eq!(receiver.decrypt(&packet[2..]), Err(Error::Authentication));
        } else {
            let plain = hex(&vector["plain"]);
            assert_eq!(receiver.decrypt(&packet[2..]).unwrap(), plain);
            if i < 3 { assert_eq!(sender.encrypt(&plain).unwrap(), packet); }
        }
    }
}

#[test]
fn malformed_and_oversized_control_records_do_not_change_chaining_state() {
    let data = vectors();
    let mut cipher = control::Cipher::new(&hex(&data["control_blob"]), &hex(&data["auth_key"])).unwrap();
    assert_eq!(cipher.decrypt(&[0; 31]), Err(Error::InvalidPacket));
    assert_eq!(cipher.encrypt(&vec![0; 65535]), Err(Error::InvalidPacket));
    let first = &data["records"][0];
    assert_eq!(cipher.encrypt(&[]).unwrap(), hex(&first["packet"]));
    assert_eq!(cipher.decrypt(&hex(&first["packet"])[2..]).unwrap(), b"");
}
