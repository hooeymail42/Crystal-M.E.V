use anyhow::{Result, anyhow};
use solana_sdk::pubkey::Pubkey;

/// Lifinity V2 AMM pool state (oracle-adjusted constant product)
#[derive(Debug, Clone)]
pub struct LifinityAmmInfo {
    pub pool_mint: Pubkey,
    pub token_a_mint: Pubkey,
    pub token_b_mint: Pubkey,
    pub token_a_vault: Pubkey,
    pub token_b_vault: Pubkey,
    pub pool_fee_numerator: u64,
    pub pool_fee_denominator: u64,
    pub oracle_main_account: Pubkey,
    pub oracle_sub_account: Pubkey,
    pub oracle_pc_account: Pubkey,
}

impl LifinityAmmInfo {
    pub fn try_deserialize(data: &[u8]) -> Result<Self> {
        // 8 (disc) + 32*3 (mints) + 32*2 (vaults) + 8*2 (fees) + 32*3 (oracles)
        let min_len = 8 + 32 * 3 + 32 * 2 + 8 * 2 + 32 * 3;
        if data.len() < min_len {
            return Err(anyhow!("Data too short for LifinityAmmInfo: {} bytes", data.len()));
        }

        let d = &data[8..]; // skip discriminator
        let mut offset = 0;

        macro_rules! read_pubkey {
            () => {{
                let mut bytes = [0u8; 32];
                bytes.copy_from_slice(&d[offset..offset+32]);
                offset += 32;
                Pubkey::new_from_array(bytes)
            }};
        }

        macro_rules! read_u64 {
            () => {{
                let val = u64::from_le_bytes(d[offset..offset+8].try_into().unwrap());
                offset += 8;
                val
            }};
        }

        let pool_mint = read_pubkey!();
        let token_a_mint = read_pubkey!();
        let token_b_mint = read_pubkey!();
        let token_a_vault = read_pubkey!();
        let token_b_vault = read_pubkey!();
        let pool_fee_numerator = read_u64!();
        let pool_fee_denominator = read_u64!();
        let oracle_main_account = read_pubkey!();
        let oracle_sub_account = read_pubkey!();
        let oracle_pc_account = read_pubkey!();

        Ok(Self {
            pool_mint,
            token_a_mint,
            token_b_mint,
            token_a_vault,
            token_b_vault,
            pool_fee_numerator,
            pool_fee_denominator,
            oracle_main_account,
            oracle_sub_account,
            oracle_pc_account,
        })
    }

    /// Buy tokens (token_b -> token_a) using oracle-adjusted constant product with fee.
    /// Formula: out = (in * (1 - fee) * reserve_a) / (reserve_b + in * (1 - fee))
    pub fn calculate_buy_output(&self, sol_amount_in: u64, sol_reserve: u64, token_reserve: u64) -> u64 {
        if sol_amount_in == 0 || sol_reserve == 0 || token_reserve == 0 {
            return 0;
        }

        let fee_num = self.pool_fee_numerator as u128;
        let fee_den = self.pool_fee_denominator as u128;

        if fee_den == 0 {
            return 0;
        }

        // amount after fee
        let amount_after_fee = (sol_amount_in as u128) * (fee_den - fee_num) / fee_den;

        // constant product
        let new_sol_reserve = sol_reserve as u128 + amount_after_fee;
        let new_token_reserve = (sol_reserve as u128) * (token_reserve as u128) / new_sol_reserve;

        let tokens_out = (token_reserve as u128).saturating_sub(new_token_reserve);
        tokens_out.min(token_reserve as u128) as u64
    }

    /// Sell tokens (token_a -> token_b) using oracle-adjusted constant product with fee.
    pub fn calculate_sell_output(&self, token_amount_in: u64, sol_reserve: u64, token_reserve: u64) -> u64 {
        if token_amount_in == 0 || sol_reserve == 0 || token_reserve == 0 {
            return 0;
        }

        let fee_num = self.pool_fee_numerator as u128;
        let fee_den = self.pool_fee_denominator as u128;

        if fee_den == 0 {
            return 0;
        }

        let new_token_reserve = token_reserve as u128 + token_amount_in as u128;
        let new_sol_reserve = (sol_reserve as u128) * (token_reserve as u128) / new_token_reserve;

        let sol_out = (sol_reserve as u128).saturating_sub(new_sol_reserve);

        // Apply fee on output
        let sol_after_fee = sol_out * (fee_den - fee_num) / fee_den;
        sol_after_fee.min(sol_reserve as u128) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_pool() -> LifinityAmmInfo {
        LifinityAmmInfo {
            pool_mint: Pubkey::default(),
            token_a_mint: Pubkey::default(),
            token_b_mint: Pubkey::default(),
            token_a_vault: Pubkey::default(),
            token_b_vault: Pubkey::default(),
            pool_fee_numerator: 30,       // 0.3%
            pool_fee_denominator: 10_000,
            oracle_main_account: Pubkey::default(),
            oracle_sub_account: Pubkey::default(),
            oracle_pc_account: Pubkey::default(),
        }
    }

    #[test]
    fn test_lifinity_buy() {
        let pool = test_pool();
        let sol_reserve = 100_000_000_000u64; // 100 SOL
        let token_reserve = 1_000_000_000_000u64; // 1T tokens
        let out = pool.calculate_buy_output(1_000_000_000, sol_reserve, token_reserve); // buy with 1 SOL
        assert!(out > 0);
        assert!(out < token_reserve);
    }

    #[test]
    fn test_lifinity_sell() {
        let pool = test_pool();
        let sol_reserve = 100_000_000_000u64;
        let token_reserve = 1_000_000_000_000u64;
        let out = pool.calculate_sell_output(10_000_000_000, sol_reserve, token_reserve); // sell 10B tokens
        assert!(out > 0);
        assert!(out < sol_reserve);
    }

    #[test]
    fn test_lifinity_zero_input() {
        let pool = test_pool();
        assert_eq!(pool.calculate_buy_output(0, 100, 100), 0);
        assert_eq!(pool.calculate_sell_output(0, 100, 100), 0);
    }

    #[test]
    fn test_lifinity_roundtrip_loses_value() {
        let pool = test_pool();
        let sol_reserve = 100_000_000_000u64;
        let token_reserve = 1_000_000_000_000u64;
        let tokens = pool.calculate_buy_output(1_000_000_000, sol_reserve, token_reserve);
        let new_sol_reserve = sol_reserve + 1_000_000_000 * 9970 / 10000;
        let new_token_reserve = token_reserve - tokens;
        let sol_back = pool.calculate_sell_output(tokens, new_sol_reserve, new_token_reserve);
        assert!(sol_back < 1_000_000_000); // Lost to fees
    }
}
