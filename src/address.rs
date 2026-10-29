use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

pub fn crc16_ccitt(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

pub fn friendly_address(
    workchain: i8,
    account_id: [u8; 32],
    bounceable: bool,
    testnet: bool,
) -> String {
    URL_SAFE_NO_PAD.encode(friendly_address_raw(
        workchain, account_id, bounceable, testnet,
    ))
}

pub fn friendly_address_bytes(
    workchain: i8,
    account_id: [u8; 32],
    bounceable: bool,
    testnet: bool,
) -> [u8; 48] {
    let raw = friendly_address_raw(workchain, account_id, bounceable, testnet);
    let mut encoded = [0u8; 48];
    let written = URL_SAFE_NO_PAD
        .encode_slice(raw, &mut encoded)
        .expect("36-byte friendly address fits in 48 base64url bytes");
    debug_assert_eq!(written, encoded.len());
    encoded
}

fn friendly_address_raw(
    workchain: i8,
    account_id: [u8; 32],
    bounceable: bool,
    testnet: bool,
) -> [u8; 36] {
    let mut raw = [0u8; 36];
    let mut tag = if bounceable { 0x11 } else { 0x51 };
    if testnet {
        tag |= 0x80;
    }
    raw[0] = tag;
    raw[1] = workchain as u8;
    raw[2..34].copy_from_slice(&account_id);
    let crc = crc16_ccitt(&raw[..34]);
    raw[34..36].copy_from_slice(&crc.to_be_bytes());
    raw
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn crc_and_friendly_address_are_deterministic() {
        let a = friendly_address(0, [0u8; 32], true, false);
        assert_eq!(a, "EQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAM9c");
        assert_eq!(
            friendly_address_bytes(0, [0u8; 32], true, false),
            *a.as_bytes()
        );
        let t = friendly_address(0, [0u8; 32], true, true);
        assert_eq!(t, "kQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAHTW");
    }
}
