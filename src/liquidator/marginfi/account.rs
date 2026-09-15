//! MarginFi v2 account deserialization: `MarginfiAccount`, `Bank`, and the
//! `WrappedI80F48` fixed-point type.
//!
//! # ⚠️ Layout verification (READ BEFORE ENABLING REAL EXECUTION)
//!
//! MarginFi accounts are Anchor accounts: an 8-byte discriminator followed by
//! Borsh-packed fields. The field ORDER below follows the published MarginFi v2
//! IDL, but the exact padding between fields has changed across program upgrades.
//! The absolute offsets grouped in [`layout`] MUST be confirmed against a live
//! mainnet account before trusting decoded values with real funds — the same
//! discipline `CLAUDE.md` documents for the Raydium/DAMM pool layouts.
//!
//! To verify: fetch a real account with `getAccountInfo` (base64), and assert
//! the decoded `mint`, `mint_decimals`, `group`, and share values match what the
//! MarginFi UI / SDK reports for that account. `Bank::from_account_data` and
//! `MarginfiAccount::from_account_data` are written so that only the constants in
//! [`layout`] need to change if an upgrade shifts the layout.

use anyhow::{anyhow, bail, Result};
use solana_sdk::pubkey::Pubkey;

use super::constants::ANCHOR_DISCRIMINATOR_LEN;

/// MarginFi's fixed-point number: I80F48 stored as a little-endian 16-byte
/// two's-complement integer scaled by 2^48.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WrappedI80F48 {
    pub bytes: [u8; 16],
}

impl WrappedI80F48 {
    const FRACTIONAL_BITS: i32 = 48;

    pub fn from_slice(s: &[u8]) -> Result<Self> {
        if s.len() < 16 {
            bail!("WrappedI80F48 needs 16 bytes, got {}", s.len());
        }
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&s[..16]);
        Ok(Self { bytes })
    }

    /// Convert to f64. Precision is sufficient for USD valuation and health
    /// comparisons; do NOT round-trip this back into on-chain amounts.
    pub fn to_f64(&self) -> f64 {
        let raw = i128::from_le_bytes(self.bytes);
        (raw as f64) / (2f64).powi(Self::FRACTIONAL_BITS)
    }
}

/// A single balance slot in a `MarginfiAccount`'s lending account.
#[derive(Debug, Clone)]
pub struct Balance {
    pub active: bool,
    pub bank_pk: Pubkey,
    /// Shares in the asset side; multiply by the bank's `asset_share_value`
    /// to get native token amount.
    pub asset_shares: WrappedI80F48,
    /// Shares in the liability side; multiply by the bank's
    /// `liability_share_value` to get native token amount owed.
    pub liability_shares: WrappedI80F48,
}

impl Balance {
    pub fn is_empty(&self) -> bool {
        !self.active
            || (self.asset_shares.to_f64() == 0.0 && self.liability_shares.to_f64() == 0.0)
    }
}

/// A decoded MarginFi user account. Only the fields required for liquidation
/// scoring are retained.
#[derive(Debug, Clone)]
pub struct MarginfiAccount {
    pub group: Pubkey,
    pub authority: Pubkey,
    pub balances: Vec<Balance>,
}

/// A decoded MarginFi bank. Only fields required for valuation, health, and the
/// liquidate instruction are retained.
#[derive(Debug, Clone)]
pub struct Bank {
    pub mint: Pubkey,
    pub mint_decimals: u8,
    pub group: Pubkey,
    pub asset_share_value: WrappedI80F48,
    pub liability_share_value: WrappedI80F48,
    pub liquidity_vault: Pubkey,
    pub insurance_vault: Pubkey,
    /// Primary price oracle (Pyth/Switchboard) — first entry of `oracle_keys`.
    pub oracle_key: Pubkey,
    pub asset_weight_maint: WrappedI80F48,
    pub liability_weight_maint: WrappedI80F48,
}

impl Bank {
    pub fn asset_amount(&self, shares: &WrappedI80F48) -> f64 {
        shares.to_f64() * self.asset_share_value.to_f64()
    }

    pub fn liability_amount(&self, shares: &WrappedI80F48) -> f64 {
        shares.to_f64() * self.liability_share_value.to_f64()
    }
}

/// Absolute byte offsets (from the start of account data, discriminator
/// included). **VERIFY against a mainnet account before real execution.**
///
/// Grouping every offset here means a program upgrade that shifts the layout is
/// a single-location fix, matching the repo's convention for pool layouts.
pub mod layout {
    use super::ANCHOR_DISCRIMINATOR_LEN;

    // --- MarginfiAccount ---
    pub const ACCT_GROUP: usize = ANCHOR_DISCRIMINATOR_LEN; // 8
    pub const ACCT_AUTHORITY: usize = ACCT_GROUP + 32; // 40
    pub const ACCT_LENDING_ACCOUNT: usize = ACCT_AUTHORITY + 32; // 72

    /// Number of balance slots in a lending account.
    pub const BALANCE_COUNT: usize = 16;
    /// Stride between consecutive balance slots.
    pub const BALANCE_STRIDE: usize = 104;
    // Offsets within a single balance slot:
    pub const BAL_ACTIVE: usize = 0;
    pub const BAL_BANK_PK: usize = 8;
    pub const BAL_ASSET_SHARES: usize = 40;
    pub const BAL_LIABILITY_SHARES: usize = 56;

    // --- Bank ---
    pub const BANK_MINT: usize = ANCHOR_DISCRIMINATOR_LEN; // 8
    pub const BANK_MINT_DECIMALS: usize = BANK_MINT + 32; // 40
    pub const BANK_GROUP: usize = BANK_MINT_DECIMALS + 1 + 7; // 48 (1 byte + 7 pad)
    pub const BANK_ASSET_SHARE_VALUE: usize = BANK_GROUP + 32; // 80
    pub const BANK_LIABILITY_SHARE_VALUE: usize = BANK_ASSET_SHARE_VALUE + 16; // 96
    pub const BANK_LIQUIDITY_VAULT: usize = BANK_LIABILITY_SHARE_VALUE + 16; // 112
    pub const BANK_INSURANCE_VAULT: usize = BANK_LIQUIDITY_VAULT + 32 + 2; // 146 (+2 bump bytes)
    // BankConfig block — weights and oracle keys. These sit inside the nested
    // `config: BankConfig`. Offsets are the least-stable part of the layout.
    pub const BANK_ASSET_WEIGHT_MAINT: usize = 0x120; // VERIFY
    pub const BANK_LIABILITY_WEIGHT_MAINT: usize = 0x140; // VERIFY
    pub const BANK_ORACLE_KEY_0: usize = 0x1A0; // VERIFY
}

fn read_pubkey(data: &[u8], off: usize) -> Result<Pubkey> {
    let end = off
        .checked_add(32)
        .ok_or_else(|| anyhow!("pubkey offset overflow"))?;
    let slice = data
        .get(off..end)
        .ok_or_else(|| anyhow!("pubkey out of range at {}", off))?;
    let mut b = [0u8; 32];
    b.copy_from_slice(slice);
    Ok(Pubkey::new_from_array(b))
}

fn read_i80f48(data: &[u8], off: usize) -> Result<WrappedI80F48> {
    let end = off
        .checked_add(16)
        .ok_or_else(|| anyhow!("i80f48 offset overflow"))?;
    let slice = data
        .get(off..end)
        .ok_or_else(|| anyhow!("i80f48 out of range at {}", off))?;
    WrappedI80F48::from_slice(slice)
}

impl MarginfiAccount {
    /// Decode a `MarginfiAccount` from raw account data (discriminator included).
    pub fn from_account_data(data: &[u8]) -> Result<Self> {
        use layout::*;
        if data.len() < ACCT_LENDING_ACCOUNT + BALANCE_COUNT * BALANCE_STRIDE {
            bail!(
                "account data too short for MarginfiAccount: {} bytes",
                data.len()
            );
        }
        let group = read_pubkey(data, ACCT_GROUP)?;
        let authority = read_pubkey(data, ACCT_AUTHORITY)?;

        let mut balances = Vec::with_capacity(BALANCE_COUNT);
        for i in 0..BALANCE_COUNT {
            let base = ACCT_LENDING_ACCOUNT + i * BALANCE_STRIDE;
            let active = data
                .get(base + BAL_ACTIVE)
                .map(|b| *b != 0)
                .unwrap_or(false);
            let bank_pk = read_pubkey(data, base + BAL_BANK_PK)?;
            let asset_shares = read_i80f48(data, base + BAL_ASSET_SHARES)?;
            let liability_shares = read_i80f48(data, base + BAL_LIABILITY_SHARES)?;
            balances.push(Balance {
                active,
                bank_pk,
                asset_shares,
                liability_shares,
            });
        }

        Ok(Self {
            group,
            authority,
            balances,
        })
    }

    /// Balances with any non-zero position.
    pub fn active_balances(&self) -> impl Iterator<Item = &Balance> {
        self.balances.iter().filter(|b| !b.is_empty())
    }
}

impl Bank {
    /// Decode a `Bank` from raw account data (discriminator included).
    pub fn from_account_data(data: &[u8]) -> Result<Self> {
        use layout::*;
        if data.len() < BANK_ORACLE_KEY_0 + 32 {
            bail!("account data too short for Bank: {} bytes", data.len());
        }
        let mint = read_pubkey(data, BANK_MINT)?;
        let mint_decimals = *data
            .get(BANK_MINT_DECIMALS)
            .ok_or_else(|| anyhow!("bank mint_decimals out of range"))?;
        let group = read_pubkey(data, BANK_GROUP)?;
        let asset_share_value = read_i80f48(data, BANK_ASSET_SHARE_VALUE)?;
        let liability_share_value = read_i80f48(data, BANK_LIABILITY_SHARE_VALUE)?;
        let liquidity_vault = read_pubkey(data, BANK_LIQUIDITY_VAULT)?;
        let insurance_vault = read_pubkey(data, BANK_INSURANCE_VAULT)?;
        let asset_weight_maint = read_i80f48(data, BANK_ASSET_WEIGHT_MAINT)?;
        let liability_weight_maint = read_i80f48(data, BANK_LIABILITY_WEIGHT_MAINT)?;
        let oracle_key = read_pubkey(data, BANK_ORACLE_KEY_0)?;

        Ok(Self {
            mint,
            mint_decimals,
            group,
            asset_share_value,
            liability_share_value,
            liquidity_vault,
            insurance_vault,
            oracle_key,
            asset_weight_maint,
            liability_weight_maint,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i80f48_zero_and_one() {
        let zero = WrappedI80F48 { bytes: [0u8; 16] };
        assert_eq!(zero.to_f64(), 0.0);

        // 1.0 == 2^48 in raw units.
        let mut one = [0u8; 16];
        one[..16].copy_from_slice(&(1i128 << 48).to_le_bytes());
        assert!((WrappedI80F48 { bytes: one }.to_f64() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn i80f48_negative() {
        let mut neg = [0u8; 16];
        neg.copy_from_slice(&(-(1i128 << 48)).to_le_bytes());
        assert!((WrappedI80F48 { bytes: neg }.to_f64() + 1.0).abs() < 1e-12);
    }

    #[test]
    fn i80f48_fractional() {
        // 1.5 == 3 * 2^47
        let mut v = [0u8; 16];
        v.copy_from_slice(&((3i128 << 47)).to_le_bytes());
        assert!((WrappedI80F48 { bytes: v }.to_f64() - 1.5).abs() < 1e-12);
    }

    #[test]
    fn short_data_rejected() {
        assert!(MarginfiAccount::from_account_data(&[0u8; 10]).is_err());
        assert!(Bank::from_account_data(&[0u8; 10]).is_err());
    }
}
