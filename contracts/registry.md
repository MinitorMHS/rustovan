# Official TON basic-wallet registry

The official TON wallet-history page publishes the following current code hashes. The Rust implementation delegates actual code/data-cell construction to the `ton` crate; these hashes are recorded here as an audit/reference table.

| Version | Code hash |
|---|---|
| V1R1 | `a0cfc2c48aee16a271f2cfc0b7382d81756cecb1017d077faaab3bb602f6868c` |
| V1R2 | `d4902fcc9fad74698fa8e353220a68da0dcf72e32bcb2eb9ee04217c17d3062c` |
| V1R3 | `587cc789eff1c84f46ec3797e45fc809a14ff5ae24f1e0c7a6a99cc9dc9061ff` |
| V2R1 | `5c9a5e68c108e18721a07c42f9956bfb39ad77ec6d624b60c576ec88eee65329` |
| V2R2 | `fe9530d3243853083ef2ef0b4c2908c0abf6fa1c31ea243aacaa5bf8c7d753f1` |
| V3R1 | `b61041a58a7980b946e8fb9e198e3c904d24799ffa36574ea4251c41a566f581` |
| V3R2 | `84dafa449f98a6987789ba232358072bc0f76dc4524002a5d0918b9a75d2d599` |
| V4R1 | `64dd54805522c5be8a9db59cea0105ccf0d08786ca79beb8cb79e880a8d7322d` |
| V4R2 | `feb5ff6820e2ff0d9483e7e0d62c817d846789fb4ae580c878866d959dabd5c0` |
| V5R1 / W5 | `20834b7b72b112147e1b2fb457b84e74d1a30f04f737d4f62a668e9552d2b72f` |

V5R1/W5 is the latest official general-purpose wallet in the TON wallet history. No V5R2 is assumed by this project.

## Specialized contracts

The upstream Rust TON wallet enum also exposes Highload wallet contracts. They are intentionally not included in the normal vanity-wallet selector because they are specialized contract families rather than ordinary user wallet versions. The architecture can add them later with explicit wallet-ID/data-layout rules.

## Sources

- Official TON wallet history: `https://docs.ton.org/contracts/standard/wallets/history`
- Official TEP-3 wallet recommendations: `https://github.com/ton-blockchain/TEPs/blob/master/text/0003-wallets.md`
- TON Rust SDK `WalletVersion` / code-cell registry: `https://docs.rs/ton/latest/ton/ton_wallet/enum.WalletVersion.html`
