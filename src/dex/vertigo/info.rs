use anyhow::Result;
use borsh::{BorshDeserialize, BorshSerialize};
use solana_sdk::pubkey::Pubkey;

use super::constants::vertigo_program_id;

#[derive(Debug, BorshDeserialize, BorshSerialize)]
pub struct VertigoPool {
    pub mint_a: Pubkey,
    pub mint_b: Pubkey,
    pub owner: Pubkey,
}

impl VertigoPool {
    pub fn try_deserialize(data: &mut &[u8]) -> Result<Self> {
        Self::try_from_slice(data)
            .map_err(|e| anyhow::anyhow!("Failed to deserialize VertigoPool: {}", e))
    }
}

#[derive(Debug)]
pub struct VertigoInfo {
    pub mint_a: Pubkey,
    pub mint_b: Pubkey,
    pub pool: Pubkey,
}

impl VertigoInfo {
    pub fn load_checked(data: &[u8], pool: &Pubkey) -> Result<Self> {
        let mut data_slice = &data[..];
        let vertigo_pool = VertigoPool::try_deserialize(&mut data_slice)?;

        Ok(Self {
            mint_a: vertigo_pool.mint_a,
            mint_b: vertigo_pool.mint_b,
            pool: pool.to_owned(),
        })
    }

    pub fn get_token_and_sol_vaults(&self, base_mint: &str, _sol_mint: &Pubkey) -> (Pubkey, Pubkey) {
        let token_x_vault = if base_mint == self.mint_a.to_string() {
            derive_vault_address(&self.pool, &self.mint_b).0
        } else {
            derive_vault_address(&self.pool, &self.mint_a).0
        };

        let token_base_vault = if base_mint == self.mint_a.to_string() {
            derive_vault_address(&self.pool, &self.mint_a).0
        } else {
            derive_vault_address(&self.pool, &self.mint_b).0
        };

        (token_x_vault, token_base_vault)
    }

    /// Vertigo uses a constant product AMM with 0.3% fee
    pub fn calculate_swap_output(
        amount_in: u64,
        reserve_in: u64,
        reserve_out: u64,
        fee_bps: u64,
    ) -> u64 {
        if reserve_in == 0 || reserve_out == 0 || amount_in == 0 {
            return 0;
        }

        let amount_in_with_fee = (amount_in as u128) * (10000 - fee_bps) as u128;
        let numerator = amount_in_with_fee * (reserve_out as u128);
        let denominator = (reserve_in as u128) * 10000 + amount_in_with_fee;

        if denominator == 0 { return 0; }
        (numerator / denominator) as u64
    }
}

/// Helper function to derive vault PDA
pub fn derive_vault_address(pool: &Pubkey, mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[pool.as_ref(), mint.as_ref()], &vertigo_program_id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vertigo_swap_basic() {
        let out = VertigoInfo::calculate_swap_output(
            1_000_000_000,
            500_000_000_000,
            500_000_000_000,
            30,
        );
        assert!(out > 0);
        assert!(out < 500_000_000_000);
    }

    #[test]
    fn test_vertigo_swap_zero() {
        assert_eq!(VertigoInfo::calculate_swap_output(0, 100, 100, 30), 0);
        assert_eq!(VertigoInfo::calculate_swap_output(100, 0, 100, 30), 0);
    }

    #[test]
    fn test_vertigo_fee_impact() {
        let low = VertigoInfo::calculate_swap_output(1_000_000, 100_000_000, 100_000_000, 10);
        let high = VertigoInfo::calculate_swap_output(1_000_000, 100_000_000, 100_000_000, 100);
        assert!(low > high);
    }
}
