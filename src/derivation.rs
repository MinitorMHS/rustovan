use anyhow::{Result, anyhow};
use bip39::Mnemonic;
use ed25519_dalek::{SigningKey, VerifyingKey};
use hmac::{Hmac, Mac};
use sha2::Sha512;
use unicode_normalization::UnicodeNormalization;
use zeroize::{Zeroize, Zeroizing};

type HmacSha512 = Hmac<Sha512>;

/// Normalize a BIP39 passphrase to NFKD before GPU PBKDF2.
/// ASCII callers can bypass this because NFKD leaves ASCII unchanged.
pub fn normalize_bip39_passphrase(passphrase: &str) -> Zeroizing<String> {
    Zeroizing::new(passphrase.nfkd().collect())
}

/// Derive a BIP39 seed while keeping a normalized Unicode passphrase in a
/// zeroizing allocation. ASCII passphrases are already NFKD and avoid a copy.
pub fn derive_bip39_seed(mnemonic: &Mnemonic, passphrase: &str) -> Zeroizing<[u8; 64]> {
    let normalized = if passphrase.is_ascii() {
        None
    } else {
        Some(normalize_bip39_passphrase(passphrase))
    };
    let normalized_passphrase = normalized
        .as_ref()
        .map_or(passphrase, |value| value.as_str());
    Zeroizing::new(mnemonic.to_seed_normalized(normalized_passphrase))
}

/// Derive the TON SLIP-0010 Ed25519 private key from a BIP39 seed.
pub fn derive_ton_private_key_from_seed(
    seed: &[u8; 64],
    account: u32,
) -> Result<Zeroizing<[u8; 32]>> {
    // Reject high-bit values rather than silently aliasing a lower account.
    if account > 0x7fff_ffff {
        return Err(anyhow!("account index must be <= 2147483647"));
    }

    let mut mac =
        HmacSha512::new_from_slice(b"ed25519 seed").map_err(|_| anyhow!("HMAC init failed"))?;
    mac.update(seed);
    let mut out = mac.finalize().into_bytes();
    let mut key = Zeroizing::new([0u8; 32]);
    let mut chain = Zeroizing::new([0u8; 32]);
    key.copy_from_slice(&out[..32]);
    chain.copy_from_slice(&out[32..]);
    out.zeroize();

    for index in [
        44u32 | 0x8000_0000,
        607u32 | 0x8000_0000,
        account | 0x8000_0000,
    ] {
        let mut m =
            HmacSha512::new_from_slice(&chain[..]).map_err(|_| anyhow!("HMAC init failed"))?;
        m.update(&[0]);
        m.update(&key[..]);
        m.update(&index.to_be_bytes());
        let mut out = m.finalize().into_bytes();
        key.copy_from_slice(&out[..32]);
        chain.copy_from_slice(&out[32..]);
        out.zeroize();
    }
    Ok(key)
}

/// CPU reference: BIP39 -> seed -> SLIP-0010 Ed25519 at m/44'/607'/account'.
pub fn derive_ton_private_key(
    mnemonic: &Mnemonic,
    passphrase: &str,
    account: u32,
) -> Result<Zeroizing<[u8; 32]>> {
    let seed = derive_bip39_seed(mnemonic, passphrase);
    derive_ton_private_key_from_seed(&seed, account)
}

pub fn derive_ton_public_key(private_key: &[u8; 32]) -> [u8; 32] {
    let sk = SigningKey::from_bytes(private_key);
    let vk: VerifyingKey = sk.verifying_key();
    vk.to_bytes()
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_account_values_that_alias_hardened_paths() {
        let seed = [0u8; 64];
        assert!(derive_ton_private_key_from_seed(&seed, 0x8000_0000).is_err());
        assert!(derive_ton_private_key_from_seed(&seed, u32::MAX).is_err());
        assert!(derive_ton_private_key_from_seed(&seed, 0).is_ok());
    }

    #[test]
    fn bip39_passphrase_acceptance_vector_uses_real_decomposed_unicode() {
        let composed = "passphrase-ñ";
        let decomposed = "passphrase-n\u{303}";
        let ascii_escape_text = "passphrase-n\\u{303}";
        assert_eq!(composed.len(), 13);
        assert_eq!(decomposed.len(), 14);
        assert_eq!(ascii_escape_text.len(), 19);
        assert_eq!(
            normalize_bip39_passphrase(composed),
            normalize_bip39_passphrase(decomposed)
        );
        assert_ne!(
            normalize_bip39_passphrase(composed),
            normalize_bip39_passphrase(ascii_escape_text)
        );

        let mnemonic = Mnemonic::parse_in_normalized(
            bip39::Language::English,
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        )
        .unwrap();
        assert_eq!(mnemonic.to_seed(composed), mnemonic.to_seed(decomposed));
        assert_ne!(
            mnemonic.to_seed(composed),
            mnemonic.to_seed(ascii_escape_text)
        );
    }

    #[test]
    fn zeroizing_seed_helper_matches_bip39_for_ascii_and_unicode_passphrases() {
        let mnemonic = Mnemonic::parse_in_normalized(
            bip39::Language::English,
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        )
        .unwrap();
        for passphrase in ["", "TREZOR", "passphrase-ñ", "passphrase-n\u{303}"] {
            assert_eq!(
                derive_bip39_seed(&mnemonic, passphrase)[..],
                mnemonic.to_seed(passphrase)[..]
            );
        }
    }

    #[test]
    fn bip39_and_slip10_match_known_account_zero_vector() {
        let mnemonic = Mnemonic::parse_in_normalized(
            bip39::Language::English,
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        ).unwrap();
        let key = derive_ton_private_key(&mnemonic, "TREZOR", 0).unwrap();
        assert_eq!(
            hex::encode(&key[..]),
            "d0f0c5522593343e960933db00c1df873c0c1f87771c853c2bf0ef23b2f97b41"
        );
        // Fixed Ed25519 public-key reference vector.
        assert_eq!(
            hex::encode(derive_ton_public_key(&key)),
            "3a2da797ef192099b8d918e55e5e2c54ea228637fdec66d77264ac22e447db4a"
        );
    }
    #[test]
    fn bip39_slip10_ed25519_match_independent_account_vectors() {
        let mnemonic = Mnemonic::parse_in_normalized(
            bip39::Language::English,
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        )
        .unwrap();
        let seed = Zeroizing::new(mnemonic.to_seed("TREZOR"));
        assert_eq!(
            hex::encode(&seed[..]),
            "c55257c360c07c72029aebc1b53c05ed0362ada38ead3e3e9efa3708e53495531f09a6987599d18264c1e1c92f2cf141630c7a3c4ab7c81b2f001698e7463b04"
        );

        // Fixed reference vectors for BIP39 seed and SLIP-0010 Ed25519 derivation.
        let vectors = [
            (
                0u32,
                "d0f0c5522593343e960933db00c1df873c0c1f87771c853c2bf0ef23b2f97b41",
                "3a2da797ef192099b8d918e55e5e2c54ea228637fdec66d77264ac22e447db4a",
            ),
            (
                1u32,
                "155b5252db97fa44be8ce3982b5b70cde3444c8dd60322809c1c8dd208524327",
                "de32e72e80405bad44e12918f19d9ff83a98874feb8b35c9e767dd264e30acd6",
            ),
            (
                42u32,
                "d64201f2394785813d7c0bce17cd85c2b8731f10a313477e2421fba9d877d02d",
                "69f96454788eae783e8d15aa041d8f87d6a1135562c4db05faf91df03e7c4a71",
            ),
            (
                32_767u32,
                "914f87e70d5fbda0218501b24f062883934feebf8d9fa827f26f6f40c77bb924",
                "a437dde013bf8eaff19f60ddecf975ef33fb36cb2eab673fd9590da805304c5a",
            ),
            (
                2_147_483_647u32,
                "292ecc54647b78669a04da59684b4f56070dde8c1154d1daa4b40d2e21c54dc1",
                "b17910187535bb4208550bcb7c9a1b451da9397ae1113c3809795711e84699ca",
            ),
        ];

        for (account, expected_private, expected_public) in vectors {
            let private = derive_ton_private_key_from_seed(&seed, account).unwrap();
            assert_eq!(
                hex::encode(&private[..]),
                expected_private,
                "account {account}"
            );
            assert_eq!(
                hex::encode(derive_ton_public_key(&private)),
                expected_public,
                "account {account}"
            );
        }
    }
}
