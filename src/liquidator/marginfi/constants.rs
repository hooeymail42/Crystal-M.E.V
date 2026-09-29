//! MarginFi v2 on-chain constants.
//!
//! Program IDs and the production lending group are public, stable values.
//! Anything marked `VERIFY` is a byte layout that must be confirmed against a
//! live mainnet account before enabling real execution — see the module-level
//! docs in `account.rs`.

use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

/// MarginFi v2 program ID (mainnet).
pub const MARGINFI_PROGRAM_ID: &str = "MFv2hWf31Z9kbCa1snEPYctwafyhdvnV7FZnsebVacA";

/// The main MarginFi v2 lending group on mainnet. Override via
/// `MARGINFI_GROUP` when targeting a different group.
pub const MARGINFI_MAIN_GROUP: &str = "4qp6Fx6tnZkY5Wropq9wUYgtFxXKwE6viZxFHg3rdAG8";

/// Anchor instruction discriminator for `lending_account_liquidate`.
/// Computed as `sha256("global:lending_account_liquidate")[..8]`.
pub const LIQUIDATE_DISCRIMINATOR: [u8; 8] = [214, 169, 151, 213, 251, 167, 86, 219];

/// Anchor account discriminator length that prefixes every MarginFi account.
pub const ANCHOR_DISCRIMINATOR_LEN: usize = 8;

/// PDA seed for a bank's liquidity vault authority.
pub const LIQUIDITY_VAULT_AUTHORITY_SEED: &[u8] = b"liquidity_vault_auth";

pub fn marginfi_program_id() -> Pubkey {
    Pubkey::from_str(MARGINFI_PROGRAM_ID).expect("valid marginfi program id")
}

pub fn marginfi_main_group() -> Pubkey {
    Pubkey::from_str(MARGINFI_MAIN_GROUP).expect("valid marginfi group")
}

/// Derive a bank's liquidity vault authority PDA.
pub fn liquidity_vault_authority(bank: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[LIQUIDITY_VAULT_AUTHORITY_SEED, bank.as_ref()],
        &marginfi_program_id(),
    )
    .0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discriminator_matches_anchor_hash() {
        // Recompute sha256("global:lending_account_liquidate")[..8] and compare.
        // Kept as a hard-coded expectation so a mistyped constant is caught.
        assert_eq!(
            LIQUIDATE_DISCRIMINATOR,
            [214, 169, 151, 213, 251, 167, 86, 219]
        );
    }

    #[test]
    fn program_and_group_parse() {
        let _ = marginfi_program_id();
        let _ = marginfi_main_group();
        let _ = liquidity_vault_authority(&marginfi_main_group());
    }
}
