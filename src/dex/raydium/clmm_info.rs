#![allow(dead_code)]
use anyhow::{Result, anyhow};
use solana_sdk::pubkey::Pubkey;

/// Raydium CLMM (Concentrated Liquidity Market Maker) pool state
#[derive(Debug, Clone)]
pub struct RaydiumClmmInfo {
    pub amm_config: Pubkey,
    pub pool_creator: Pubkey,
    pub token_mint_0: Pubkey,
    pub token_mint_1: Pubkey,
    pub token_vault_0: Pubkey,
    pub token_vault_1: Pubkey,
    pub observation_key: Pubkey,
    pub mint_decimals_0: u8,
    pub mint_decimals_1: u8,
    pub tick_spacing: u16,
    pub liquidity: u128,
    pub sqrt_price_x64: u128,
    pub tick_current: i32,
    pub protocol_fees_token_0: u64,
    pub protocol_fees_token_1: u64,
    pub fund_fees_token_0: u64,
    pub fund_fees_token_1: u64,
    pub trade_fee_rate: u32,
    pub status: u8,
}

impl RaydiumClmmInfo {
    /// Deserialize from on-chain account data.
    /// Layout after 8-byte discriminator:
    ///   bump(1), amm_config(32), pool_creator(32),
    ///   token_mint_0(32), token_mint_1(32),
    ///   token_vault_0(32), token_vault_1(32),
    ///   observation_key(32),
    ///   mint_decimals_0(1), mint_decimals_1(1),
    ///   tick_spacing(2), liquidity(16), sqrt_price_x64(16),
    ///   tick_current(4), padding(2),
    ///   fee_growth_global_0_x64(16), fee_growth_global_1_x64(16),
    ///   protocol_fees_token_0(8), protocol_fees_token_1(8),
    ///   swap_in_amount_token_0(16), swap_out_amount_token_1(16),
    ///   swap_in_amount_token_1(16), swap_out_amount_token_0(16),
    ///   status(1), ...padding..., fund_fees_token_0(8), fund_fees_token_1(8),
    ///   open_time(8), recent_epoch(8), trade_fee_rate(4)...
    #[allow(unused_assignments)]
    pub fn try_deserialize(data: &[u8]) -> Result<Self> {
        if data.len() < 400 {
            return Err(anyhow!("Data too short for RaydiumClmmInfo: {} bytes", data.len()));
        }

        let d = &data[8..]; // skip discriminator
        let mut offset = 0;

        macro_rules! read_u8 {
            () => {{
                let val = d[offset];
                offset += 1;
                val
            }};
        }

        macro_rules! read_u16 {
            () => {{
                let val = u16::from_le_bytes(d[offset..offset+2].try_into().unwrap());
                offset += 2;
                val
            }};
        }

        macro_rules! read_u32 {
            () => {{
                let val = u32::from_le_bytes(d[offset..offset+4].try_into().unwrap());
                offset += 4;
                val
            }};
        }

        macro_rules! read_u64 {
            () => {{
                let val = u64::from_le_bytes(d[offset..offset+8].try_into().unwrap());
                offset += 8;
                val
            }};
        }

        macro_rules! read_u128 {
            () => {{
                let val = u128::from_le_bytes(d[offset..offset+16].try_into().unwrap());
                offset += 16;
                val
            }};
        }

        macro_rules! read_i32 {
            () => {{
                let val = i32::from_le_bytes(d[offset..offset+4].try_into().unwrap());
                offset += 4;
                val
            }};
        }

        macro_rules! read_pubkey {
            () => {{
                let mut bytes = [0u8; 32];
                bytes.copy_from_slice(&d[offset..offset+32]);
                offset += 32;
                Pubkey::new_from_array(bytes)
            }};
        }

        let _bump = read_u8!();
        let amm_config = read_pubkey!();
        let pool_creator = read_pubkey!();
        let token_mint_0 = read_pubkey!();
        let token_mint_1 = read_pubkey!();
        let token_vault_0 = read_pubkey!();
        let token_vault_1 = read_pubkey!();
        let observation_key = read_pubkey!();
        let mint_decimals_0 = read_u8!();
        let mint_decimals_1 = read_u8!();
        let tick_spacing = read_u16!();
        let liquidity = read_u128!();
        let sqrt_price_x64 = read_u128!();
        let tick_current = read_i32!();
        let _padding = read_u16!();

        // fee_growth_global (2 x u128)
        let _fee_growth_global_0 = read_u128!();
        let _fee_growth_global_1 = read_u128!();

        let protocol_fees_token_0 = read_u64!();
        let protocol_fees_token_1 = read_u64!();

        // swap amounts (4 x u128)
        let _swap_in_0 = read_u128!();
        let _swap_out_1 = read_u128!();
        let _swap_in_1 = read_u128!();
        let _swap_out_0 = read_u128!();

        let status = read_u8!();

        // Skip padding to reach fund fees and trade_fee_rate
        // The exact padding varies; read fund fees from known relative positions
        let fund_fees_token_0 = if d.len() > offset + 16 {
            let v = u64::from_le_bytes(d[offset..offset+8].try_into().unwrap_or([0u8; 8]));
            offset += 8;
            v
        } else { 0 };
        let fund_fees_token_1 = if d.len() > offset + 8 {
            let v = u64::from_le_bytes(d[offset..offset+8].try_into().unwrap_or([0u8; 8]));
            offset += 8;
            v
        } else { 0 };

        // Skip open_time(8) + recent_epoch(8) to get trade_fee_rate
        let trade_fee_rate = if d.len() > offset + 20 {
            offset += 16; // skip open_time + recent_epoch
            read_u32!()
        } else {
            2500 // default 0.25% = 2500/1_000_000
        };

        Ok(Self {
            amm_config,
            pool_creator,
            token_mint_0,
            token_mint_1,
            token_vault_0,
            token_vault_1,
            observation_key,
            mint_decimals_0,
            mint_decimals_1,
            tick_spacing,
            liquidity,
            sqrt_price_x64,
            tick_current,
            protocol_fees_token_0,
            protocol_fees_token_1,
            fund_fees_token_0,
            fund_fees_token_1,
            trade_fee_rate,
            status,
        })
    }

    /// Swap token_0 -> token_1 (price decreases, a_to_b).
    /// Uses concentrated liquidity formula within a single tick range:
    ///   new_sqrt_price = L * sqrt_price / (L + delta_0 * sqrt_price)
    ///   delta_1 = L * (sqrt_price - new_sqrt_price) / 2^64
    pub fn calculate_swap_0_to_1(&self, amount_in: u64) -> ClmmSwapResult {
        if self.liquidity == 0 || self.sqrt_price_x64 == 0 || amount_in == 0 {
            return ClmmSwapResult::zero();
        }

        // Apply fee
        let fee_amount = (amount_in as u128) * (self.trade_fee_rate as u128) / 1_000_000;
        let amount_after_fee = (amount_in as u128).saturating_sub(fee_amount);

        let l = self.liquidity;
        let sqrt_p = self.sqrt_price_x64;

        // new_sqrt_price = L * sqrt_price / (L + delta_0 * sqrt_price / 2^64)
        let denominator = l as u128 + (amount_after_fee * sqrt_p) / (1u128 << 64);
        if denominator == 0 {
            return ClmmSwapResult::zero();
        }

        let new_sqrt_price = (l as u128) * sqrt_p / denominator;

        // delta_1 = L * (sqrt_price - new_sqrt_price) / 2^64
        let delta_1 = if sqrt_p > new_sqrt_price {
            (l as u128) * (sqrt_p - new_sqrt_price) / (1u128 << 64)
        } else {
            0
        };

        ClmmSwapResult {
            amount_in: amount_in,
            amount_out: delta_1 as u64,
            fee_amount: fee_amount as u64,
        }
    }

    /// Swap token_1 -> token_0 (price increases, b_to_a).
    /// new_sqrt_price = sqrt_price + delta_1 * 2^64 / L
    /// delta_0 = L * (1/sqrt_price - 1/new_sqrt_price) * 2^64
    pub fn calculate_swap_1_to_0(&self, amount_in: u64) -> ClmmSwapResult {
        if self.liquidity == 0 || self.sqrt_price_x64 == 0 || amount_in == 0 {
            return ClmmSwapResult::zero();
        }

        let fee_amount = (amount_in as u128) * (self.trade_fee_rate as u128) / 1_000_000;
        let amount_after_fee = (amount_in as u128).saturating_sub(fee_amount);

        let l = self.liquidity;
        let sqrt_p = self.sqrt_price_x64;

        // new_sqrt_price = sqrt_price + delta_1 * 2^64 / L
        let new_sqrt_price = sqrt_p + (amount_after_fee * (1u128 << 64)) / (l as u128);

        // delta_0 = L * 2^64 * (new_sqrt_price - sqrt_price) / (sqrt_price * new_sqrt_price)
        let numerator = (l as u128) * (new_sqrt_price - sqrt_p);
        let denominator = sqrt_p / (1u128 << 32) * new_sqrt_price / (1u128 << 32);
        let delta_0 = if denominator > 0 {
            numerator / denominator
        } else {
            0
        };

        ClmmSwapResult {
            amount_in: amount_in,
            amount_out: delta_0 as u64,
            fee_amount: fee_amount as u64,
        }
    }

    /// Simple constant-product fallback using reserves
    pub fn calculate_swap_simple(
        amount_in: u64,
        reserve_in: u64,
        reserve_out: u64,
        fee_rate: u32,
    ) -> u64 {
        if reserve_in == 0 || reserve_out == 0 || amount_in == 0 {
            return 0;
        }
        let fee_factor = 1_000_000u128 - fee_rate as u128;
        let amount_with_fee = (amount_in as u128) * fee_factor;
        let numerator = amount_with_fee * (reserve_out as u128);
        let denominator = (reserve_in as u128) * 1_000_000 + amount_with_fee;
        if denominator == 0 { return 0; }
        (numerator / denominator) as u64
    }
}

#[derive(Debug, Clone)]
pub struct ClmmSwapResult {
    pub amount_in: u64,
    pub amount_out: u64,
    pub fee_amount: u64,
}

impl ClmmSwapResult {
    pub fn zero() -> Self {
        Self { amount_in: 0, amount_out: 0, fee_amount: 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_pool() -> RaydiumClmmInfo {
        RaydiumClmmInfo {
            amm_config: Pubkey::default(),
            pool_creator: Pubkey::default(),
            token_mint_0: Pubkey::default(),
            token_mint_1: Pubkey::default(),
            token_vault_0: Pubkey::default(),
            token_vault_1: Pubkey::default(),
            observation_key: Pubkey::default(),
            mint_decimals_0: 9,
            mint_decimals_1: 6,
            tick_spacing: 60,
            liquidity: 10_000_000_000_000,
            sqrt_price_x64: 1u128 << 64, // price = 1.0
            tick_current: 0,
            protocol_fees_token_0: 0,
            protocol_fees_token_1: 0,
            fund_fees_token_0: 0,
            fund_fees_token_1: 0,
            trade_fee_rate: 2500, // 0.25%
            status: 1,
        }
    }

    #[test]
    fn test_clmm_swap_0_to_1() {
        let pool = test_pool();
        let result = pool.calculate_swap_0_to_1(1_000_000_000); // 1 token
        assert!(result.amount_out > 0);
        assert!(result.fee_amount > 0);
        assert!(result.amount_out < 1_000_000_000);
    }

    #[test]
    fn test_clmm_swap_1_to_0() {
        let pool = test_pool();
        let result = pool.calculate_swap_1_to_0(1_000_000_000);
        assert!(result.amount_out > 0);
        assert!(result.fee_amount > 0);
    }

    #[test]
    fn test_clmm_swap_zero_input() {
        let pool = test_pool();
        let result = pool.calculate_swap_0_to_1(0);
        assert_eq!(result.amount_out, 0);
    }

    #[test]
    fn test_clmm_swap_zero_liquidity() {
        let mut pool = test_pool();
        pool.liquidity = 0;
        let result = pool.calculate_swap_0_to_1(1_000_000);
        assert_eq!(result.amount_out, 0);
    }

    #[test]
    fn test_clmm_simple_fallback() {
        let out = RaydiumClmmInfo::calculate_swap_simple(
            1_000_000_000,
            500_000_000_000,
            500_000_000_000,
            2500,
        );
        assert!(out > 0);
        assert!(out < 500_000_000_000);
    }

    #[test]
    fn test_clmm_fee_impact() {
        let mut pool = test_pool();
        let low_fee = pool.calculate_swap_0_to_1(1_000_000_000);
        pool.trade_fee_rate = 10000; // 1%
        let high_fee = pool.calculate_swap_0_to_1(1_000_000_000);
        assert!(low_fee.amount_out > high_fee.amount_out);
    }
}
