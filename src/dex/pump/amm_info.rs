#![allow(dead_code)]
use anyhow::{Result, anyhow};
use solana_sdk::pubkey::Pubkey;

/// Pump.fun bonding curve pool state
#[derive(Debug, Clone)]
pub struct PumpAmmInfo {
    pub virtual_sol_reserves: u64,
    pub virtual_token_reserves: u64,
    pub real_sol_reserves: u64,
    pub real_token_reserves: u64,
    pub token_total_supply: u64,
    pub complete: bool,
    pub mint: Pubkey,
}

impl PumpAmmInfo {
    pub fn try_deserialize(data: &[u8]) -> Result<Self> {
        if data.len() < 8 + 8 * 4 + 8 + 1 + 32 {
            return Err(anyhow!("Data too short for PumpAmmInfo: {} bytes", data.len()));
        }

        let d = &data[8..]; // skip discriminator
        let mut offset = 0;

        macro_rules! read_u64 {
            () => {{
                let val = u64::from_le_bytes(d[offset..offset+8].try_into().unwrap());
                offset += 8;
                val
            }};
        }

        let virtual_sol_reserves = read_u64!();
        let virtual_token_reserves = read_u64!();
        let real_sol_reserves = read_u64!();
        let real_token_reserves = read_u64!();
        let token_total_supply = read_u64!();
        let complete = d[offset] != 0;
        offset += 1;
        let mut mint_bytes = [0u8; 32];
        mint_bytes.copy_from_slice(&d[offset..offset+32]);
        let mint = Pubkey::new_from_array(mint_bytes);

        Ok(Self {
            virtual_sol_reserves,
            virtual_token_reserves,
            real_sol_reserves,
            real_token_reserves,
            token_total_supply,
            complete,
            mint,
        })
    }

    /// Buy tokens with SOL on the bonding curve
    /// Uses virtual reserves for pricing, 1% fee
    pub fn calculate_buy_output(&self, sol_amount_in: u64) -> u64 {
        if self.complete || sol_amount_in == 0 || self.virtual_sol_reserves == 0 || self.virtual_token_reserves == 0 {
            return 0;
        }

        // 1% fee
        let fee = sol_amount_in / 100;
        let sol_after_fee = sol_amount_in.saturating_sub(fee);

        // Constant product with virtual reserves
        let new_virtual_sol = self.virtual_sol_reserves as u128 + sol_after_fee as u128;
        let new_virtual_token = (self.virtual_sol_reserves as u128)
            * (self.virtual_token_reserves as u128)
            / new_virtual_sol;

        let tokens_out = (self.virtual_token_reserves as u128).saturating_sub(new_virtual_token);

        // Cap at real token reserves
        let tokens_out = tokens_out.min(self.real_token_reserves as u128);
        tokens_out as u64
    }

    /// Sell tokens for SOL on the bonding curve
    /// Uses virtual reserves for pricing, 1% fee
    pub fn calculate_sell_output(&self, token_amount_in: u64) -> u64 {
        if self.complete || token_amount_in == 0 || self.virtual_sol_reserves == 0 || self.virtual_token_reserves == 0 {
            return 0;
        }

        let new_virtual_token = self.virtual_token_reserves as u128 + token_amount_in as u128;
        let new_virtual_sol = (self.virtual_sol_reserves as u128)
            * (self.virtual_token_reserves as u128)
            / new_virtual_token;

        let sol_out = (self.virtual_sol_reserves as u128).saturating_sub(new_virtual_sol);

        // 1% fee
        let fee = sol_out / 100;
        let sol_after_fee = sol_out.saturating_sub(fee);

        // Cap at real sol reserves
        let sol_after_fee = sol_after_fee.min(self.real_sol_reserves as u128);
        sol_after_fee as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_pool() -> PumpAmmInfo {
        PumpAmmInfo {
            virtual_sol_reserves: 30_000_000_000, // 30 SOL
            virtual_token_reserves: 1_000_000_000_000, // 1B tokens (6 decimals)
            real_sol_reserves: 10_000_000_000, // 10 SOL
            real_token_reserves: 500_000_000_000, // 500M tokens
            token_total_supply: 1_000_000_000_000,
            complete: false,
            mint: Pubkey::default(),
        }
    }

    #[test]
    fn test_pump_buy() {
        let pool = test_pool();
        let out = pool.calculate_buy_output(1_000_000_000); // buy with 1 SOL
        assert!(out > 0);
        assert!(out < pool.real_token_reserves);
    }

    #[test]
    fn test_pump_sell() {
        let pool = test_pool();
        let out = pool.calculate_sell_output(10_000_000_000); // sell 10B tokens
        assert!(out > 0);
        assert!(out < pool.real_sol_reserves);
    }

    #[test]
    fn test_pump_complete_pool() {
        let mut pool = test_pool();
        pool.complete = true;
        assert_eq!(pool.calculate_buy_output(1_000_000_000), 0);
        assert_eq!(pool.calculate_sell_output(1_000_000_000), 0);
    }

    #[test]
    fn test_pump_roundtrip_loses_value() {
        let pool = test_pool();
        let tokens = pool.calculate_buy_output(1_000_000_000);
        // Simulate updated pool state after buy
        let pool_after = PumpAmmInfo {
            virtual_sol_reserves: pool.virtual_sol_reserves + 990_000_000, // after 1% fee
            virtual_token_reserves: pool.virtual_token_reserves - tokens,
            real_sol_reserves: pool.real_sol_reserves + 990_000_000,
            real_token_reserves: pool.real_token_reserves - tokens,
            ..pool
        };
        let sol_back = pool_after.calculate_sell_output(tokens);
        assert!(sol_back < 1_000_000_000); // Lost to fees
    }
}
