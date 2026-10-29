use anyhow::{Result, anyhow};
use std::sync::OnceLock;
use zeroize::Zeroizing;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    Mainnet,
    Testnet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalletVersion {
    V1R1,
    V1R2,
    V1R3,
    V2R1,
    V2R2,
    V3R1,
    V3R2,
    V4R1,
    V4R2,
    V5R1,
    WalletTg,
}

impl WalletVersion {
    pub const ALL: [Self; 11] = [
        Self::V1R1,
        Self::V1R2,
        Self::V1R3,
        Self::V2R1,
        Self::V2R2,
        Self::V3R1,
        Self::V3R2,
        Self::V4R1,
        Self::V4R2,
        Self::V5R1,
        Self::WalletTg,
    ];
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "v1r1" => Ok(Self::V1R1),
            "v1r2" => Ok(Self::V1R2),
            "v1r3" => Ok(Self::V1R3),
            "v2r1" => Ok(Self::V2R1),
            "v2r2" => Ok(Self::V2R2),
            "v3r1" => Ok(Self::V3R1),
            "v3r2" => Ok(Self::V3R2),
            "v4r1" => Ok(Self::V4R1),
            "v4r2" | "v4" => Ok(Self::V4R2),
            "v5r1" | "w5" | "v5" => Ok(Self::V5R1),
            "wallettg" | "wallet-tg" | "telegram" | "tg" => Ok(Self::WalletTg),
            _ => Err(anyhow!("unsupported wallet version: {s}")),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::V1R1 => "v1r1",
            Self::V1R2 => "v1r2",
            Self::V1R3 => "v1r3",
            Self::V2R1 => "v2r1",
            Self::V2R2 => "v2r2",
            Self::V3R1 => "v3r1",
            Self::V3R2 => "v3r2",
            Self::V4R1 => "v4r1",
            Self::V4R2 => "v4r2",
            Self::V5R1 => "v5r1",
            Self::WalletTg => "wallettg",
        }
    }
    fn ton(self) -> Option<ton::ton_wallet::WalletVersion> {
        Some(match self {
            Self::V1R1 => ton::ton_wallet::WalletVersion::V1R1,
            Self::V1R2 => ton::ton_wallet::WalletVersion::V1R2,
            Self::V1R3 => ton::ton_wallet::WalletVersion::V1R3,
            Self::V2R1 => ton::ton_wallet::WalletVersion::V2R1,
            Self::V2R2 => ton::ton_wallet::WalletVersion::V2R2,
            Self::V3R1 => ton::ton_wallet::WalletVersion::V3R1,
            Self::V3R2 => ton::ton_wallet::WalletVersion::V3R2,
            Self::V4R1 => ton::ton_wallet::WalletVersion::V4R1,
            Self::V4R2 => ton::ton_wallet::WalletVersion::V4R2,
            Self::V5R1 => ton::ton_wallet::WalletVersion::V5R1,
            Self::WalletTg => return None,
        })
    }
}

const WALLET_TG_SUBWALLET_ID: u32 = 0x7fff_7f11;
const WALLET_TG_CODE_HASH: &str =
    "9149ae51c1e4689710cebf7830297b16acfbadb363a920a537893e7ffeeca768";
const WALLET_TG_CODE_BOC: &str =
    "b5ee9c7241010101001a000030ff00209821d7498308b9f240df8085f833d0ed1e20ed53d969427e39";

fn wallet_tg_code() -> Result<ton::ton_core::cell::TonCell> {
    static CODE: OnceLock<std::result::Result<ton::ton_core::cell::TonCell, String>> =
        OnceLock::new();
    match CODE.get_or_init(|| {
        use ton::ton_core::traits::tlb::TLB;
        let code = ton::ton_core::cell::TonCell::from_boc_hex(WALLET_TG_CODE_BOC)
            .map_err(|error| format!("could not parse WalletTg trampoline code: {error}"))?;
        let actual_hash = hex::encode(
            code.cell_hash()
                .map_err(|error| format!("could not hash WalletTg trampoline code: {error}"))?
                .as_slice(),
        );
        if actual_hash != WALLET_TG_CODE_HASH {
            return Err(format!(
                "WalletTg trampoline hash mismatch: expected {WALLET_TG_CODE_HASH}, got {actual_hash}"
            ));
        }
        Ok(code)
    }) {
        Ok(code) => Ok(code.clone()),
        Err(error) => Err(anyhow!(error.clone())),
    }
}

fn derive_wallet_tg_account_id(public_key: &[u8; 32], workchain: i8) -> Result<[u8; 32]> {
    use ton::ton_core::cell::TonCell;

    let mut builder = TonCell::builder();
    builder.write_bits([0u8], 8)?; // Initial storage revision.
    builder.write_bits([0u8; 4], 32)?; // seqno.
    builder.write_bits(WALLET_TG_SUBWALLET_ID.to_be_bytes(), 32)?;
    builder.write_bits(public_key, 256)?;
    let data = builder.build()?;
    let state_init = ton::block_tlb::StateInit::new(wallet_tg_code()?, data);
    let address = state_init.derive_address(workchain as i32)?;
    Ok(*address.hash.as_slice_sized())
}

/// Exact W5R1 wallet-id construction from the TON client context:
/// `wallet_id = network_global_id XOR (1 | wc:int8 | wallet_version:uint8 | subwallet:uint15)`.
pub fn v5r1_wallet_id(
    network: Network,
    workchain: i8,
    wallet_version: u8,
    subwallet: u32,
) -> Result<i32> {
    if subwallet >= (1 << 15) {
        return Err(anyhow!("W5 subwallet number must be < 32768"));
    }
    let network_global_id = match network {
        Network::Mainnet => -239i32,
        Network::Testnet => -3i32,
    } as u32;
    let wc = (workchain as i32 as u32) & 0xff;
    let context = (1u32 << 31) | (wc << 23) | ((wallet_version as u32) << 15) | subwallet;
    Ok((network_global_id ^ context) as i32)
}

pub fn derive_address(
    version: WalletVersion,
    private_key: &[u8; 32],
    network: Network,
    workchain: i8,
    subwallet: u32,
    bounceable: bool,
) -> Result<String> {
    let encoded = derive_friendly_address_bytes(
        version,
        private_key,
        network,
        workchain,
        subwallet,
        bounceable,
    )?;
    Ok(String::from_utf8(encoded.to_vec())
        .map_err(|_| anyhow!("friendly address encoder returned invalid UTF-8"))?)
}

pub fn derive_friendly_address_bytes(
    version: WalletVersion,
    private_key: &[u8; 32],
    network: Network,
    workchain: i8,
    subwallet: u32,
    bounceable: bool,
) -> Result<[u8; 48]> {
    use ton::ton_wallet::{KeyPair, TonWallet};
    let public_key = crate::derivation::derive_ton_public_key(private_key);
    let account_id = if version == WalletVersion::WalletTg {
        derive_wallet_tg_account_id(&public_key, workchain)?
    } else {
        let mut secret_key = Zeroizing::new([0u8; 64]);
        secret_key[..32].copy_from_slice(private_key);
        secret_key[32..].copy_from_slice(&public_key);
        let key_pair = KeyPair {
            public_key,
            secret_key: *secret_key,
        };
        let wallet_id = if version == WalletVersion::V5R1 {
            v5r1_wallet_id(network, workchain, 0, subwallet)?
        } else {
            0x29A9A317u32 as i32
        };
        let ton_version = version
            .ton()
            .ok_or_else(|| anyhow!("unsupported standard TON wallet version"))?;
        let wallet = TonWallet::new_with_params(ton_version, key_pair, workchain as i32, wallet_id)
            .map_err(|e| anyhow!("TON wallet construction failed: {e}"))?;
        *wallet.address.hash.as_slice_sized()
    };
    Ok(crate::address::friendly_address_bytes(
        workchain,
        account_id,
        bounceable,
        matches!(network, Network::Testnet),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wallettg_trampoline_and_address_match_fixed_reference_vector() {
        use bip39::Mnemonic;
        use ton::ton_core::traits::tlb::TLB;

        let mnemonic = Mnemonic::parse_in_normalized(
            bip39::Language::English,
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        )
        .unwrap();
        let private = crate::derivation::derive_ton_private_key(&mnemonic, "", 0).unwrap();
        let address = derive_address(
            WalletVersion::WalletTg,
            &private,
            Network::Mainnet,
            0,
            0,
            true,
        )
        .unwrap();
        assert_eq!(address, "EQD4d2pzr-W4jzh1wOd52HrsH7rRagzkSUPC3_jCgMxmK2IO");
        assert_eq!(
            hex::encode(wallet_tg_code().unwrap().cell_hash().unwrap().as_slice()),
            WALLET_TG_CODE_HASH
        );
    }

    #[test]
    fn wallettg_is_listed_and_parses_aliases() {
        assert_eq!(
            WalletVersion::parse("wallettg").unwrap(),
            WalletVersion::WalletTg
        );
        assert_eq!(
            WalletVersion::parse("telegram").unwrap(),
            WalletVersion::WalletTg
        );
        assert_eq!(WalletVersion::ALL.len(), 11);
        assert!(WalletVersion::ALL.contains(&WalletVersion::WalletTg));
        assert_eq!(WalletVersion::WalletTg.name(), "wallettg");
    }
    #[test]
    fn w5_default_ids_match_ton_context() {
        assert_eq!(
            v5r1_wallet_id(Network::Mainnet, 0, 0, 0).unwrap() as u32,
            0x7fff_ff11
        );
        assert_eq!(
            v5r1_wallet_id(Network::Testnet, 0, 0, 0).unwrap() as u32,
            0x7fff_fffd
        );
        assert_eq!(
            v5r1_wallet_id(Network::Mainnet, 0, 0, 1).unwrap() as u32,
            0x7fff_ff10
        );
    }
    #[test]
    fn w5_rejects_out_of_range_subwallet() {
        assert!(v5r1_wallet_id(Network::Mainnet, 0, 0, 32768).is_err());
    }

    #[test]
    fn upstream_address_fixtures_match_and_all_versions_construct() {
        const TON_MNEMONIC: &str = "fancy carpet hello mandate penalty trial consider property top vicious exit rebuild tragic profit urban major total month holiday sudden rib gather media vicious";
        const TON_MNEMONIC_V5: &str = "section garden tomato dinner season dice renew length useful spin trade intact use universe what post spike keen mandate behind concert egg doll rug";

        fn key_material(phrase: &str) -> Zeroizing<[u8; 32]> {
            let mnemonic = ton::ton_wallet::Mnemonic::from_str(phrase, None).unwrap();
            let pair = mnemonic.to_key_pair().unwrap();
            let mut private = Zeroizing::new([0u8; 32]);
            private.copy_from_slice(&pair.secret_key[..32]);
            private
        }

        let private = key_material(TON_MNEMONIC);
        for version in WalletVersion::ALL {
            let address = derive_address(version, &private, Network::Mainnet, 0, 0, true).unwrap();
            assert_eq!(address.len(), 48, "{}", version.name());
        }
        assert_eq!(
            derive_address(WalletVersion::V3R1, &private, Network::Mainnet, 0, 0, true).unwrap(),
            "EQBiMfDMivebQb052Z6yR3jHrmwNhw1kQ5bcAUOBYsK_VPuK"
        );
        assert_eq!(
            derive_address(WalletVersion::V3R2, &private, Network::Mainnet, 0, 0, true).unwrap(),
            "EQA-RswW9QONn88ziVm4UKnwXDEot5km7GEEXsfie_0TFOCO"
        );
        assert_eq!(
            derive_address(WalletVersion::V4R2, &private, Network::Mainnet, 0, 0, true).unwrap(),
            "EQCDM_QGggZ3qMa_f3lRPk4_qLDnLTqdi6OkMAV2NB9r5TG3"
        );

        let private_v5 = key_material(TON_MNEMONIC_V5);
        assert_eq!(
            derive_address(
                WalletVersion::V5R1,
                &private_v5,
                Network::Mainnet,
                0,
                0,
                false
            )
            .unwrap(),
            "UQDv2YSmlrlLH3hLNOVxC8FcQf4F9eGNs4vb2zKma4txo6i3"
        );
        assert_eq!(
            derive_address(
                WalletVersion::V5R1,
                &private_v5,
                Network::Testnet,
                0,
                0,
                false
            )
            .unwrap(),
            "0QA_6fh0aRAkD7n1MNfAUx8TvyCUw2iTQfzVM-0isMze2anN"
        );
    }
    #[test]
    fn bip39_wallet_address_regression_matrix() {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

        let mnemonic = bip39::Mnemonic::parse_in_normalized(
            bip39::Language::English,
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        )
        .unwrap();
        let private = crate::derivation::derive_ton_private_key(&mnemonic, "TREZOR", 0).unwrap();

        // Fixed regression snapshots from the deterministic BIP39 vector.
        // Independent TON-SDK comparison remains a separate release gate.
        let fixtures = [
            (
                WalletVersion::V1R1,
                "EQDCAJlOpt9t8byv1EHvqC3Wwrplow4nc4B11HPX3EpF1sS9",
            ),
            (
                WalletVersion::V1R2,
                "EQAzpYhNwuDCvMhJEplWivmKvmGCkNSKp1I3bS8vYfMQNhgi",
            ),
            (
                WalletVersion::V1R3,
                "EQDJp3JaLgR9T489cHbd3Ak01q3ULCJc4x2tMrxxFlRC5w68",
            ),
            (
                WalletVersion::V2R1,
                "EQBsg8O4eCBkwpi9sdBTwAF79x59H4yB6o_e-81ufOVY7V3c",
            ),
            (
                WalletVersion::V2R2,
                "EQB5K_E-o4QIbQvXLGTGtXEfE72beHN3hLQu7NferJHgxKB8",
            ),
            (
                WalletVersion::V3R1,
                "EQB4H0wqpcQhkYLHv59yLvZ7rAuNxPkoPR83WgGneYUc81Mf",
            ),
            (
                WalletVersion::V3R2,
                "EQDDA7NyLJVAxItk51d-9JzZftn-AbOfYrH0eUZ7LjkB8J5E",
            ),
            (
                WalletVersion::V4R1,
                "EQAxGu16v4a2e7V_Zs2KJY9fpUJTw5tSNiX8g36Xu2Pda26_",
            ),
            (
                WalletVersion::V4R2,
                "EQC8Yak9NGmaRtppVSD6FAU8ZKvTRIVBLJ18UP9mc2vwdOjX",
            ),
            (
                WalletVersion::V5R1,
                "EQBL9EHEd5TsCF64mP6CVW0W6BGzR0D-aAFMOdVE9RgwQ5AU",
            ),
        ];

        for (version, expected_mainnet_bounceable) in fixtures {
            let mainnet_bounceable =
                derive_address(version, &private, Network::Mainnet, 0, 0, true).unwrap();
            assert_eq!(
                mainnet_bounceable,
                expected_mainnet_bounceable,
                "{}",
                version.name()
            );

            let mainnet_non_bounceable =
                derive_address(version, &private, Network::Mainnet, 0, 0, false).unwrap();
            let testnet_bounceable =
                derive_address(version, &private, Network::Testnet, 0, 0, true).unwrap();
            let testnet_non_bounceable =
                derive_address(version, &private, Network::Testnet, 0, 0, false).unwrap();

            let decode = |address: &str| URL_SAFE_NO_PAD.decode(address).unwrap();
            let main_bounce = decode(&mainnet_bounceable);
            let main_nonbounce = decode(&mainnet_non_bounceable);
            let test_bounce = decode(&testnet_bounceable);
            let test_nonbounce = decode(&testnet_non_bounceable);

            assert_eq!(
                main_bounce[0],
                0x11,
                "{} mainnet bounceable tag",
                version.name()
            );
            assert_eq!(
                main_nonbounce[0],
                0x51,
                "{} mainnet non-bounceable tag",
                version.name()
            );
            assert_eq!(
                test_bounce[0],
                0x91,
                "{} testnet bounceable tag",
                version.name()
            );
            assert_eq!(
                test_nonbounce[0],
                0xd1,
                "{} testnet non-bounceable tag",
                version.name()
            );

            assert_eq!(&main_bounce[2..34], &main_nonbounce[2..34]);
            assert_eq!(&test_bounce[2..34], &test_nonbounce[2..34]);
            if version != WalletVersion::V5R1 {
                assert_eq!(&main_bounce[2..34], &test_bounce[2..34]);
            } else {
                assert_eq!(
                    testnet_bounceable,
                    "kQADZhpztO51FeE6nxI06ClLo2PGMG0KhwHkKTq2zWTd7tdS"
                );
            }

            for bytes in [main_bounce, main_nonbounce, test_bounce, test_nonbounce] {
                assert_eq!(bytes.len(), 36);
                let crc = crate::address::crc16_ccitt(&bytes[..34]);
                assert_eq!(&bytes[34..36], &crc.to_be_bytes());
            }
        }
    }
}
