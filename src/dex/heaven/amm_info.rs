use anyhow::{Result, anyhow};
use solana_sdk::pubkey::Pubkey;

use super::constants::SNIPER_TAX_DURATION_SECS;

/// Heaven AMM pool state (bonding curve with time-decaying sniper tax)
#[derive(Debug, Clone)]
pub struct HeavenAmmInfo {
    pub virtual_sol_reserves: u64,
    pub virtual_token_reserves: u64,
    pub real_sol_reserves: u64,
    pub real_token_reserves: u64,
    pub complete: bool,
    pub mint: Pubkey,
    pub launch_timestamp: i64,
}

impl HeavenAmmInfo {
    pub fn try_deserialize(data: &[u8]) -> Result<Self> {
        // 8 (disc) + 8*4 (reserves) + 1 (complete) + 32 (mint) + 8 (timestamp)
        let min_len = 8 + 8 * 4 + 1 + 32 + 8;
        if data.len() < min_len {
            return Err(anyhow!("Data too short for HeavenAmmInfo: {} bytes", data.len()));
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
        let complete = d[offset] != 0;
        offset += 1;
        let mut mint_bytes = [0u8; 32];
        mint_bytes.copy_from_slice(&d[offset..offset+32]);
        let mint = Pubkey::new_from_array(mint_bytes);
        offset += 32;
        let launch_timestamp = i64::from_le_bytes(d[offset..offset+8].try_into().unwrap());

        Ok(Self {
            virtual_sol_reserves,
            virtual_token_reserves,
            real_sol_reserves,
            real_token_reserves,
            complete,
            mint,
            launch_timestamp,
        })
    }

    /// Calculate the effective fee rate including sniper tax.
    /// Sniper tax = max(0, 1 - elapsed/6) * 0.05 (5% max, decaying to 0 over 6s)
    /// Base fee = 1%
    fn effective_fee_rate(&self, current_timestamp: i64) -> f64 {
        let elapsed = (current_timestamp - self.launch_timestamp).max(0) as f64;
        let sniper_tax = if elapsed < SNIPER_TAX_DURATION_SECS as f64 {
            (1.0 - elapsed / SNIPER_TAX_DURATION_SECS as f64) * 0.05
        } else {
            0.0
        };
        0.01 + sniper_tax // 1% base + sniper tax
    }

    /// Buy tokens with SOL on the bonding curve with sniper tax
    pub fn calculate_buy_output(&self, sol_amount_in: u64, current_timestamp: i64) -> u64 {
        if self.complete || sol_amount_in == 0 || self.virtual_sol_reserves == 0 || self.virtual_token_reserves == 0 {
            return 0;
        }

        let fee_rate = self.effective_fee_rate(current_timestamp);
        let fee = (sol_amount_in as f64 * fee_rate) as u64;
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

    /// Sell tokens for SOL on the bonding curve with sniper tax
    pub fn calculate_sell_output(&self, token_amount_in: u64, current_timestamp: i64) -> u64 {
        if self.complete || token_amount_in == 0 || self.virtual_sol_reserves == 0 || self.virtual_token_reserves == 0 {
            return 0;
        }

        let new_virtual_token = self.virtual_token_reserves as u128 + token_amount_in as u128;
        let new_virtual_sol = (self.virtual_sol_reserves as u128)
            * (self.virtual_token_reserves as u128)
            / new_virtual_token;

        let sol_out = (self.virtual_sol_reserves as u128).saturating_sub(new_virtual_sol);

        let fee_rate = self.effective_fee_rate(current_timestamp);
        let fee = (sol_out as f64 * fee_rate) as u128;
        let sol_after_fee = sol_out.saturating_sub(fee);

        // Cap at real sol reserves
        let sol_after_fee = sol_after_fee.min(self.real_sol_reserves as u128);
        sol_after_fee as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_pool() -> HeavenAmmInfo {
        HeavenAmmInfo {
            virtual_sol_reserves: 30_000_000_000,     // 30 SOL
            virtual_token_reserves: 1_000_000_000_000, // 1B tokens
            real_sol_reserves: 10_000_000_000,         // 10 SOL
            real_token_reserves: 500_000_000_000,      // 500M tokens
            complete: false,
            mint: Pubkey::default(),
            launch_timestamp: 1000,
        }
    }

    #[test]
    fn test_heaven_buy_no_sniper_tax() {
        let pool = test_pool();
        // 10 seconds after launch: no sniper tax
        let out = pool.calculate_buy_output(1_000_000_000, 1010);
        assert!(out > 0);
        assert!(out < pool.real_token_reserves);
    }

    #[test]
    fn test_heaven_buy_with_sniper_tax() {
        let pool = test_pool();
        // 0 seconds after launch: max sniper tax (5% + 1% = 6%)
        let out_sniped = pool.calculate_buy_output(1_000_000_000, 1000);
        // 10 seconds after: only base fee (1%)
        let out_normal = pool.calculate_buy_output(1_000_000_000, 1010);
        // Sniped should get fewer tokens due to higher fee
        assert!(out_sniped < out_normal);
    }

    #[test]
    fn test_heaven_sell() {
        let pool = test_pool();
        let out = pool.calculate_sell_output(10_000_000_000, 1010);
        assert!(out > 0);
        assert!(out < pool.real_sol_reserves);
    }

    #[test]
    fn test_heaven_complete_pool() {
        let mut pool = test_pool();
        pool.complete = true;
        assert_eq!(pool.calculate_buy_output(1_000_000_000, 1010), 0);
        assert_eq!(pool.calculate_sell_output(1_000_000_000, 1010), 0);
    }

    #[test]
    fn test_heaven_roundtrip_loses_value() {
        let pool = test_pool();
        let ts = 1010; // no sniper tax
        let tokens = pool.calculate_buy_output(1_000_000_000, ts);
        let pool_after = HeavenAmmInfo {
            virtual_sol_reserves: pool.virtual_sol_reserves + 990_000_000,
            virtual_token_reserves: pool.virtual_token_reserves - tokens,
            real_sol_reserves: pool.real_sol_reserves + 990_000_000,
            real_token_reserves: pool.real_token_reserves - tokens,
            ..pool
        };
        let sol_back = pool_after.calculate_sell_output(tokens, ts);
        assert!(sol_back < 1_000_000_000);
    }

    #[test]
    fn test_heaven_sniper_tax_decay() {
        let pool = test_pool();
        // At launch: 6% total fee
        let fee_at_launch = pool.effective_fee_rate(1000);
        assert!((fee_at_launch - 0.06).abs() < 0.001);

        // 3 seconds in: 3.5% total fee
        let fee_midway = pool.effective_fee_rate(1003);
        assert!((fee_midway - 0.035).abs() < 0.001);

        // 6+ seconds: 1% base fee only
        let fee_after = pool.effective_fee_rate(1006);
        assert!((fee_after - 0.01).abs() < 0.001);
    }
}
