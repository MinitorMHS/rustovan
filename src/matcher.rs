use anyhow::{Result, anyhow};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use clap::ValueEnum;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum MatchMode {
    /// Keep searching until a wallet has been found for every criterion.
    #[default]
    All,
    /// Stop at the first wallet that matches any criterion.
    Any,
}

#[derive(Debug, Clone)]
pub struct Matcher {
    pub prefix: Vec<String>,
    pub suffix: Vec<String>,
    pub exact: Vec<String>,
    pub mode: MatchMode,
    pub case_sensitive: bool,
}

impl Matcher {
    pub fn criterion_count(&self) -> usize {
        self.prefix.len() + self.suffix.len() + self.exact.len()
    }

    pub fn criterion_label(&self, index: usize) -> Option<String> {
        if let Some(pattern) = self.exact.get(index) {
            return Some(format!("exact:{pattern}"));
        }
        let index = index.checked_sub(self.exact.len())?;
        if let Some(pattern) = self.prefix.get(index) {
            return Some(format!("prefix:{pattern}"));
        }
        let index = index.checked_sub(self.prefix.len())?;
        self.suffix
            .get(index)
            .map(|pattern| format!("suffix:{pattern}"))
    }

    /// Return the indexes of every individual criterion matched by an address.
    /// Index order is exact addresses, prefixes, then suffixes.
    pub fn matching_criteria(&self, address: &str) -> Vec<usize> {
        let mut matches = Vec::new();
        let mut index = 0usize;
        for exact in &self.exact {
            if address == exact {
                matches.push(index);
            }
            index += 1;
        }
        let equal = |left: &str, right: &str| {
            if self.case_sensitive {
                left == right
            } else {
                left.eq_ignore_ascii_case(right)
            }
        };
        for prefix in &self.prefix {
            if address
                .get(..prefix.len())
                .is_some_and(|head| equal(head, prefix))
            {
                matches.push(index);
            }
            index += 1;
        }
        for suffix in &self.suffix {
            if address
                .get(address.len().saturating_sub(suffix.len())..)
                .is_some_and(|tail| equal(tail, suffix))
            {
                matches.push(index);
            }
            index += 1;
        }
        matches
    }

    /// Validate patterns before starting a potentially unbounded search.
    pub fn validate(&self) -> Result<()> {
        if self.prefix.is_empty() && self.suffix.is_empty() && self.exact.is_empty() {
            return Err(anyhow!("specify a prefix, suffix, or exact TON address"));
        }
        for (name, patterns) in [
            ("prefix", &self.prefix),
            ("suffix", &self.suffix),
            ("exact address", &self.exact),
        ] {
            for pattern in patterns {
                if pattern.is_empty() {
                    return Err(anyhow!("{name} cannot be empty"));
                }
                if !pattern.is_ascii()
                    || !pattern
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                {
                    return Err(anyhow!(
                        "{name} contains characters outside the TON friendly-address alphabet"
                    ));
                }
                if pattern.len() > 48 {
                    return Err(anyhow!("{name} cannot be longer than 48 characters"));
                }
            }
        }
        for exact in &self.exact {
            if exact.len() != 48 {
                return Err(anyhow!(
                    "an exact TON friendly address must be 48 characters"
                ));
            }
            let decoded = URL_SAFE_NO_PAD
                .decode(exact)
                .map_err(|_| anyhow!("exact TON address is not valid base64url"))?;
            if decoded.len() != 36 {
                return Err(anyhow!("exact TON address must decode to 36 bytes"));
            }
            let crc = crate::address::crc16_ccitt(&decoded[..34]).to_be_bytes();
            if decoded[34] != crc[0] || decoded[35] != crc[1] {
                return Err(anyhow!("exact TON address has an invalid CRC16 checksum"));
            }
            if !matches!(decoded[0], 0x11 | 0x51 | 0x91 | 0xD1) {
                return Err(anyhow!("exact TON address has an unsupported friendly tag"));
            }
        }
        Ok(())
    }

    pub fn matches(&self, address: &str) -> bool {
        // This predicate answers whether a single candidate matched any
        // requested criterion. MatchMode::All is handled by SearchProgress,
        // which accumulates one wallet for each criterion across candidates.
        // Keep the overwhelmingly common non-match path allocation-free.
        let equal = |left: &str, right: &str| {
            if self.case_sensitive {
                left == right
            } else {
                left.eq_ignore_ascii_case(right)
            }
        };

        self.exact.iter().any(|exact| address == exact)
            || self.prefix.iter().any(|prefix| {
                address
                    .get(..prefix.len())
                    .is_some_and(|head| equal(head, prefix))
            })
            || self.suffix.iter().any(|suffix| {
                address
                    .get(address.len().saturating_sub(suffix.len())..)
                    .is_some_and(|tail| equal(tail, suffix))
            })
    }

    /// Match a stack-encoded TON friendly address without allocating a String.
    /// Search candidates are ASCII base64url, as are validated patterns.
    pub fn matches_bytes(&self, address: &[u8]) -> bool {
        let equal = |left: &[u8], right: &[u8]| {
            if self.case_sensitive {
                left == right
            } else {
                left.eq_ignore_ascii_case(right)
            }
        };

        self.exact.iter().any(|exact| address == exact.as_bytes())
            || self.prefix.iter().any(|prefix| {
                address
                    .get(..prefix.len())
                    .is_some_and(|head| equal(head, prefix.as_bytes()))
            })
            || self.suffix.iter().any(|suffix| {
                address
                    .get(address.len().saturating_sub(suffix.len())..)
                    .is_some_and(|tail| equal(tail, suffix.as_bytes()))
            })
    }
}

/// Make short vanity prefixes target the variable part of a TON friendly address.
/// A complete or partial known address header is kept literal; other values are
/// prefixed with the header selected by the current network/bounceability mode.
pub fn normalize_friendly_prefixes(prefixes: Vec<String>, active_header: &str) -> Vec<String> {
    const HEADERS: [&str; 4] = ["UQ", "EQ", "0Q", "kQ"];

    prefixes
        .into_iter()
        .map(|prefix| {
            let is_header_qualified = HEADERS.iter().any(|header| {
                prefix.len() >= header.len()
                    && prefix
                        .get(..header.len())
                        .is_some_and(|head| head.eq_ignore_ascii_case(header))
            });
            let is_partial_header = HEADERS.iter().any(|header| {
                prefix.len() < header.len()
                    && header
                        .get(..prefix.len())
                        .is_some_and(|head| head.eq_ignore_ascii_case(&prefix))
            });
            if is_header_qualified || is_partial_header {
                prefix
            } else {
                format!("{active_header}{prefix}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matcher(prefix: &[&str], suffix: &[&str], exact: &[&str], mode: MatchMode) -> Matcher {
        Matcher {
            prefix: prefix.iter().map(|s| (*s).into()).collect(),
            suffix: suffix.iter().map(|s| (*s).into()).collect(),
            exact: exact.iter().map(|s| (*s).into()).collect(),
            mode,
            case_sensitive: false,
        }
    }

    #[test]
    fn matching_is_per_criterion_and_exact_addresses_stay_byte_exact() {
        let address = "EQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAM9c";
        let matcher = matcher(&["EQ"], &["9c"], &[address], MatchMode::All);
        assert_eq!(address.len(), 48);
        assert!(matcher.validate().is_ok());
        assert!(matcher.matches(address));
        assert!(matcher.matches_bytes(address.as_bytes()));
        assert!(matcher.matches(&address.to_ascii_lowercase()));
        assert!(
            !matcher
                .matching_criteria(&address.to_ascii_lowercase())
                .contains(&0)
        );
        let other_address = "UQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAM9c";
        assert!(matcher.matches(other_address));
        let matched = matcher.matching_criteria(other_address);
        assert_eq!(matched, vec![2]);
    }

    #[test]
    fn a_candidate_matches_any_matching_criterion_in_both_search_modes() {
        let address = "EQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAM9c";
        let all = matcher(&["EQ", "UQ"], &["9c"], &[], MatchMode::All);
        let any = matcher(&["EQ", "UQ"], &["miss"], &[], MatchMode::Any);
        assert!(all.matches(address));
        assert!(any.matches(address));
        assert_eq!(all.matching_criteria(address).len(), 2);
    }

    #[test]
    fn exact_address_validation_rejects_bad_checksum_and_tag() {
        let valid = "EQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAM9c";
        let mut bad_checksum_bytes = URL_SAFE_NO_PAD.decode(valid).unwrap();
        bad_checksum_bytes[35] ^= 1;
        let bad_checksum = URL_SAFE_NO_PAD.encode(bad_checksum_bytes);

        let mut bad_tag_bytes = URL_SAFE_NO_PAD.decode(valid).unwrap();
        bad_tag_bytes[0] = 0x21;
        let checksum = crate::address::crc16_ccitt(&bad_tag_bytes[..34]);
        bad_tag_bytes[34..36].copy_from_slice(&checksum.to_be_bytes());
        let bad_tag = URL_SAFE_NO_PAD.encode(bad_tag_bytes);

        for exact in [bad_checksum, bad_tag] {
            let matcher = matcher(&[], &[], &[&exact], MatchMode::All);
            assert!(matcher.validate().is_err());
        }
    }

    #[test]
    fn rejects_empty_or_invalid_patterns() {
        assert!(matcher(&[""], &[], &[], MatchMode::All).validate().is_err());
        assert!(
            matcher(&["UQ/"], &[], &[], MatchMode::All)
                .validate()
                .is_err()
        );
        assert!(matcher(&[], &[], &[], MatchMode::All).validate().is_err());
    }

    #[test]
    fn bare_prefixes_are_scoped_after_selected_friendly_address_header() {
        let prefixes = vec!["yyy".into(), "UQabc".into(), "EQdef".into(), "U".into()];
        assert_eq!(
            normalize_friendly_prefixes(prefixes, "UQ"),
            ["UQyyy", "UQabc", "EQdef", "U"]
        );

        assert_eq!(
            normalize_friendly_prefixes(vec!["yyy".into(), "-xxx".into()], "kQ"),
            ["kQyyy", "kQ-xxx"]
        );
    }
}
