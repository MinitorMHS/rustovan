# Locked dependency license inventory

## Current status

The checked-in [package CSV](dependency-license-packages.csv) and the table in the previous version of this document were generated on 2026-10-11 for 360 packages. They are stale and must not be used as the current release license inventory.

The latest hosted dependency audit scanned 450 locked packages. That scan reports dependency security advisories, not package license metadata, so its count does not refresh the CSV or verify license expressions. See [the audit run](https://github.com/MinitorMHS/rustovan/actions/runs/41292196456). Since that run, the optional `age` dependency was pinned to an upstream revision containing crypto-bigint 0.7.10, which addresses RUSTSEC-2026-0387. The updated workflow retains one explicit ignore for RUSTSEC-2026-0374 (unmaintained basic-toml used by age's i18n macro/build tooling); a post-change audit run is still pending. This does not resolve the stale license inventory or approve licenses for sealed-output builds.

A fresh all-features metadata export could not be generated in the Windows workspace: the offline Cargo cache lacked objc2-core-graphics 0.3.2, and network access to static.crates.io was unavailable. The license inventory remains an open release gate.

## Required refresh

Regenerate the package-level inventory from the current Cargo.lock and all-features Cargo metadata. Include package name, version, SPDX license expression, source, repository, and license-file metadata. Record the exact generation command/tool version and generation date, reconcile the package count with the resolved metadata, and verify packages with missing license expressions.

The inventory is metadata, not legal advice or a compatibility decision. Maintainers must review AND/OR expressions against the distribution policy and confirm any required notices or source offers before publishing a production release.
