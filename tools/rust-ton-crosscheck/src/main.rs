use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use tonutils::wallet::{
    WALLET_V4R2_DEFAULT_ID, WALLET_V5R1_MAINNET_DEFAULT_ID, WALLET_V5R1_TESTNET_DEFAULT_ID,
    WalletV4R2, WalletV5R1, wallet_v4r2_code, wallet_v5r1_code,
};

const PUBLIC_KEY: &str = "3a2da797ef192099b8d918e55e5e2c54ea228637fdec66d77264ac22e447db4a";

fn main() -> Result<()> {
    let public_key: [u8; 32] = hex::decode(PUBLIC_KEY)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("fixture public key must be 32 bytes"))?;
    let v4 = WalletV4R2::new(public_key, WALLET_V4R2_DEFAULT_ID, wallet_v4r2_code()?, 0);
    let v5_code = wallet_v5r1_code()?;
    let fixtures = [
        ("mainnet", false, WALLET_V5R1_MAINNET_DEFAULT_ID),
        ("testnet", true, WALLET_V5R1_TESTNET_DEFAULT_ID),
    ];
    let expected_v4 = [
        [
            "EQC8Yak9NGmaRtppVSD6FAU8ZKvTRIVBLJ18UP9mc2vwdOjX",
            "UQC8Yak9NGmaRtppVSD6FAU8ZKvTRIVBLJ18UP9mc2vwdLUS",
        ],
        [
            "kQC8Yak9NGmaRtppVSD6FAU8ZKvTRIVBLJ18UP9mc2vwdFNd",
            "0QC8Yak9NGmaRtppVSD6FAU8ZKvTRIVBLJ18UP9mc2vwdA6Y",
        ],
    ];
    let expected_v5 = [
        [
            "EQBL9EHEd5TsCF64mP6CVW0W6BGzR0D-aAFMOdVE9RgwQ5AU",
            "UQBL9EHEd5TsCF64mP6CVW0W6BGzR0D-aAFMOdVE9RgwQ83R",
        ],
        [
            "kQADZhpztO51FeE6nxI06ClLo2PGMG0KhwHkKTq2zWTd7tdS",
            "0QADZhpztO51FeE6nxI06ClLo2PGMG0KhwHkKTq2zWTd7oqX",
        ],
    ];

    for (network_index, (network, test_only, wallet_id)) in fixtures.into_iter().enumerate() {
        let v5 = WalletV5R1::new(public_key, wallet_id, v5_code.clone(), 0);
        for (bounce_index, bounceable) in [true, false].into_iter().enumerate() {
            let v4_address = v4.address()?.to_string(true, true, bounceable, test_only);
            let v5_address = v5.address()?.to_string(true, true, bounceable, test_only);
            ensure!(
                v4_address == expected_v4[network_index][bounce_index],
                "V4R2 {network} bounceable={bounceable} fixture mismatch"
            );
            ensure!(
                v5_address == expected_v5[network_index][bounce_index],
                "W5 {network} bounceable={bounceable} fixture mismatch"
            );
            println!("v4r2 {network} bounceable={bounceable}: {v4_address}");
            println!("w5   {network} bounceable={bounceable}: {v5_address}");
        }
    }

    // Friendly-address flags change, but V4R2's account hash is network-independent.
    let main = URL_SAFE_NO_PAD.decode(expected_v4[0][0])?;
    let test = URL_SAFE_NO_PAD.decode(expected_v4[1][0])?;
    ensure!(
        main[2..34] == test[2..34],
        "V4R2 network flag changed account hash"
    );
    Ok(())
}
