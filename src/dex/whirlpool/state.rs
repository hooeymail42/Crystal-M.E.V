use solana_sdk::pubkey::Pubkey;
use std::io::{Error, ErrorKind, Result};

pub const NUM_REWARDS: usize = 3;

#[derive(Clone, Copy, Debug)]
pub struct Whirlpool {
    pub whirlpools_config: Pubkey, // 32
    pub whirlpool_bump: [u8; 1],   // 1

    pub tick_spacing: u16,          // 2
    pub tick_spacing_seed: [u8; 2], // 2

    pub fee_rate: u16, // 2

    pub protocol_fee_rate: u16, // 2

    pub liquidity: u128, // 16

    pub sqrt_price: u128,        // 16
    pub tick_current_index: i32, // 4

    pub protocol_fee_owed_a: u64, // 8
    pub protocol_fee_owed_b: u64, // 8

    pub token_mint_a: Pubkey,  // 32
    pub token_vault_a: Pubkey, // 32

    pub fee_growth_global_a: u128, // 16

    pub token_mint_b: Pubkey,  // 32
    pub token_vault_b: Pubkey, // 32

    pub fee_growth_global_b: u128, // 16

    pub reward_last_updated_timestamp: u64, // 8

    pub reward_infos: [WhirlpoolRewardInfo; NUM_REWARDS], // 384
}

impl Whirlpool {
    pub const LEN: usize = 8 + 261 + 384;

    /// Calculate swap output for a_to_b direction using sqrt_price math.
    /// Uses the concentrated liquidity formula:
    ///   For a->b (price decreases): delta_b = L * (sqrt_price_current - sqrt_price_new)
    ///   For b->a (price increases): delta_a = L * (1/sqrt_price_current - 1/sqrt_price_new)
    ///
    /// Simplified single-tick-range calculation (accurate when swap doesn't cross ticks).
    pub fn calculate_swap_a_to_b(&self, amount_a_in: u64, reserve_a: u64, reserve_b: u64) -> WhirlpoolSwapResult {
        if amount_a_in == 0 || self.liquidity == 0 || self.sqrt_price == 0 {
            return WhirlpoolSwapResult::default();
        }

        let fee_rate = self.fee_rate as u64;
        let fee_amount = (amount_a_in as u128 * fee_rate as u128 / 1_000_000) as u64;
        let amount_after_fee = amount_a_in.saturating_sub(fee_amount);

        // sqrt_price is stored as Q64.64 fixed point
        let sqrt_price_f = self.sqrt_price as f64 / (1u128 << 64) as f64;

        if sqrt_price_f <= 0.0 {
            return WhirlpoolSwapResult::default();
        }

        let liquidity_f = self.liquidity as f64;

        // For a->b swap: delta_a consumed, delta_b produced
        // new_sqrt_price = L * sqrt_price / (L + delta_a * sqrt_price)
        let new_sqrt_price = liquidity_f * sqrt_price_f
            / (liquidity_f + amount_after_fee as f64 * sqrt_price_f);

        if new_sqrt_price <= 0.0 || new_sqrt_price >= sqrt_price_f {
            return WhirlpoolSwapResult::default();
        }

        // delta_b = L * (sqrt_price - new_sqrt_price)
        let delta_b = liquidity_f * (sqrt_price_f - new_sqrt_price);
        let amount_b_out = delta_b as u64;

        // Cap at available reserve
        let amount_b_out = amount_b_out.min(reserve_b);

        WhirlpoolSwapResult {
            amount_in: amount_after_fee,
            amount_out: amount_b_out,
            fee_amount,
        }
    }

    /// Calculate swap output for b_to_a direction
    pub fn calculate_swap_b_to_a(&self, amount_b_in: u64, reserve_a: u64, reserve_b: u64) -> WhirlpoolSwapResult {
        if amount_b_in == 0 || self.liquidity == 0 || self.sqrt_price == 0 {
            return WhirlpoolSwapResult::default();
        }

        let fee_rate = self.fee_rate as u64;
        let fee_amount = (amount_b_in as u128 * fee_rate as u128 / 1_000_000) as u64;
        let amount_after_fee = amount_b_in.saturating_sub(fee_amount);

        let sqrt_price_f = self.sqrt_price as f64 / (1u128 << 64) as f64;

        if sqrt_price_f <= 0.0 {
            return WhirlpoolSwapResult::default();
        }

        let liquidity_f = self.liquidity as f64;

        // For b->a swap: new_sqrt_price = sqrt_price + delta_b / L
        let new_sqrt_price = sqrt_price_f + amount_after_fee as f64 / liquidity_f;

        // delta_a = L * (1/sqrt_price - 1/new_sqrt_price)
        let delta_a = liquidity_f * (1.0 / sqrt_price_f - 1.0 / new_sqrt_price);
        let amount_a_out = delta_a as u64;

        let amount_a_out = amount_a_out.min(reserve_a);

        WhirlpoolSwapResult {
            amount_in: amount_after_fee,
            amount_out: amount_a_out,
            fee_amount,
        }
    }

    /// Simple constant-product fallback for when we don't have sqrt_price data
    pub fn calculate_swap_simple(
        amount_in: u64,
        reserve_in: u64,
        reserve_out: u64,
        fee_rate_bps: u16,
    ) -> u64 {
        if reserve_in == 0 || reserve_out == 0 || amount_in == 0 {
            return 0;
        }
        // fee_rate is in hundredths of a bps (1e-6), convert to ratio
        let fee_ppm = fee_rate_bps as u128; // parts per million
        let amount_in_with_fee = (amount_in as u128) * (1_000_000 - fee_ppm);
        let numerator = amount_in_with_fee * (reserve_out as u128);
        let denominator = (reserve_in as u128) * 1_000_000 + amount_in_with_fee;
        if denominator == 0 { return 0; }
        (numerator / denominator) as u64
    }
}

#[derive(Debug, Clone, Default)]
pub struct WhirlpoolSwapResult {
    pub amount_in: u64,
    pub amount_out: u64,
    pub fee_amount: u64,
}

#[derive(Copy, Clone, Debug)]
pub struct WhirlpoolRewardInfo {
    pub mint: Pubkey,
    pub vault: Pubkey,
    pub authority: Pubkey,
    pub emissions_per_second_x64: u128,
    pub growth_global_x64: u128,
}

#[derive(Clone, Debug)]
pub struct TickArray {
    pub start_tick_index: i32,
    pub ticks: [Tick; TICK_ARRAY_SIZE],
    pub whirlpool: Pubkey,
}

#[derive(Copy, Clone, Default, Debug)]
pub struct Tick {
    pub initialized: bool,
    pub liquidity_net: i128,
    pub liquidity_gross: u128,
    pub fee_growth_outside_a: u128,
    pub fee_growth_outside_b: u128,
    pub reward_growths_outside: [u128; NUM_REWARDS],
}

impl Tick {
    pub fn check_is_valid_start_tick(tick_index: i32, tick_spacing: u16) -> bool {
        tick_index % (tick_spacing as i32 * TICK_ARRAY_SIZE as i32) == 0
    }
}

pub const TICK_ARRAY_SIZE: usize = 88;

impl Whirlpool {
    pub fn try_deserialize(data: &[u8]) -> Result<Self> {
        if data.len() < Self::LEN {
            return Err(Error::new(ErrorKind::InvalidData, "data too short for Whirlpool"));
        }

        let data = &data[8..];

        let mut offset = 0;

        let mut whirlpools_config = [0u8; 32];
        whirlpools_config.copy_from_slice(&data[offset..offset+32]);
        let whirlpools_config = Pubkey::new_from_array(whirlpools_config);
        offset += 32;

        let mut whirlpool_bump = [0u8; 1];
        whirlpool_bump.copy_from_slice(&data[offset..offset+1]);
        offset += 1;

        let tick_spacing = u16::from_le_bytes([data[offset], data[offset+1]]);
        offset += 2;

        let mut tick_spacing_seed = [0u8; 2];
        tick_spacing_seed.copy_from_slice(&data[offset..offset+2]);
        offset += 2;

        let fee_rate = u16::from_le_bytes([data[offset], data[offset+1]]);
        offset += 2;

        let protocol_fee_rate = u16::from_le_bytes([data[offset], data[offset+1]]);
        offset += 2;

        let mut liquidity_bytes = [0u8; 16];
        liquidity_bytes.copy_from_slice(&data[offset..offset+16]);
        let liquidity = u128::from_le_bytes(liquidity_bytes);
        offset += 16;

        let mut sqrt_price_bytes = [0u8; 16];
        sqrt_price_bytes.copy_from_slice(&data[offset..offset+16]);
        let sqrt_price = u128::from_le_bytes(sqrt_price_bytes);
        offset += 16;

        let mut tick_current_index_bytes = [0u8; 4];
        tick_current_index_bytes.copy_from_slice(&data[offset..offset+4]);
        let tick_current_index = i32::from_le_bytes(tick_current_index_bytes);
        offset += 4;

        let mut protocol_fee_owed_a_bytes = [0u8; 8];
        protocol_fee_owed_a_bytes.copy_from_slice(&data[offset..offset+8]);
        let protocol_fee_owed_a = u64::from_le_bytes(protocol_fee_owed_a_bytes);
        offset += 8;

        let mut protocol_fee_owed_b_bytes = [0u8; 8];
        protocol_fee_owed_b_bytes.copy_from_slice(&data[offset..offset+8]);
        let protocol_fee_owed_b = u64::from_le_bytes(protocol_fee_owed_b_bytes);
        offset += 8;

        let mut token_mint_a_bytes = [0u8; 32];
        token_mint_a_bytes.copy_from_slice(&data[offset..offset+32]);
        let token_mint_a = Pubkey::new_from_array(token_mint_a_bytes);
        offset += 32;

        let mut token_vault_a_bytes = [0u8; 32];
        token_vault_a_bytes.copy_from_slice(&data[offset..offset+32]);
        let token_vault_a = Pubkey::new_from_array(token_vault_a_bytes);
        offset += 32;

        let mut fee_growth_global_a_bytes = [0u8; 16];
        fee_growth_global_a_bytes.copy_from_slice(&data[offset..offset+16]);
        let fee_growth_global_a = u128::from_le_bytes(fee_growth_global_a_bytes);
        offset += 16;

        let mut token_mint_b_bytes = [0u8; 32];
        token_mint_b_bytes.copy_from_slice(&data[offset..offset+32]);
        let token_mint_b = Pubkey::new_from_array(token_mint_b_bytes);
        offset += 32;

        let mut token_vault_b_bytes = [0u8; 32];
        token_vault_b_bytes.copy_from_slice(&data[offset..offset+32]);
        let token_vault_b = Pubkey::new_from_array(token_vault_b_bytes);
        offset += 32;

        let mut fee_growth_global_b_bytes = [0u8; 16];
        fee_growth_global_b_bytes.copy_from_slice(&data[offset..offset+16]);
        let fee_growth_global_b = u128::from_le_bytes(fee_growth_global_b_bytes);
        offset += 16;

        let mut reward_last_updated_timestamp_bytes = [0u8; 8];
        reward_last_updated_timestamp_bytes.copy_from_slice(&data[offset..offset+8]);
        let reward_last_updated_timestamp = u64::from_le_bytes(reward_last_updated_timestamp_bytes);
        offset += 8;

        let mut reward_infos = [WhirlpoolRewardInfo {
            mint: Pubkey::default(),
            vault: Pubkey::default(),
            authority: Pubkey::default(),
            emissions_per_second_x64: 0,
            growth_global_x64: 0,
        }; NUM_REWARDS];

        for i in 0..NUM_REWARDS {
            let mut mint_bytes = [0u8; 32];
            mint_bytes.copy_from_slice(&data[offset..offset+32]);
            reward_infos[i].mint = Pubkey::new_from_array(mint_bytes);
            offset += 32;

            let mut vault_bytes = [0u8; 32];
            vault_bytes.copy_from_slice(&data[offset..offset+32]);
            reward_infos[i].vault = Pubkey::new_from_array(vault_bytes);
            offset += 32;

            let mut authority_bytes = [0u8; 32];
            authority_bytes.copy_from_slice(&data[offset..offset+32]);
            reward_infos[i].authority = Pubkey::new_from_array(authority_bytes);
            offset += 32;

            let mut emissions_bytes = [0u8; 16];
            emissions_bytes.copy_from_slice(&data[offset..offset+16]);
            reward_infos[i].emissions_per_second_x64 = u128::from_le_bytes(emissions_bytes);
            offset += 16;

            let mut growth_bytes = [0u8; 16];
            growth_bytes.copy_from_slice(&data[offset..offset+16]);
            reward_infos[i].growth_global_x64 = u128::from_le_bytes(growth_bytes);
            offset += 16;
        }

        Ok(Whirlpool {
            whirlpools_config,
            whirlpool_bump,
            tick_spacing,
            tick_spacing_seed,
            fee_rate,
            protocol_fee_rate,
            liquidity,
            sqrt_price,
            tick_current_index,
            protocol_fee_owed_a,
            protocol_fee_owed_b,
            token_mint_a,
            token_vault_a,
            fee_growth_global_a,
            token_mint_b,
            token_vault_b,
            fee_growth_global_b,
            reward_last_updated_timestamp,
            reward_infos,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_whirlpool() -> Whirlpool {
        Whirlpool {
            whirlpools_config: Pubkey::default(),
            whirlpool_bump: [0],
            tick_spacing: 64,
            tick_spacing_seed: [0; 2],
            fee_rate: 3000, // 0.3% in hundredths of bps (3000/1e6)
            protocol_fee_rate: 300,
            liquidity: 1_000_000_000_000_000, // 10^15
            sqrt_price: 18_446_744_073_709_551_616, // 1.0 in Q64.64 (2^64)
            tick_current_index: 0,
            protocol_fee_owed_a: 0,
            protocol_fee_owed_b: 0,
            token_mint_a: Pubkey::default(),
            token_vault_a: Pubkey::default(),
            fee_growth_global_a: 0,
            token_mint_b: Pubkey::default(),
            token_vault_b: Pubkey::default(),
            fee_growth_global_b: 0,
            reward_last_updated_timestamp: 0,
            reward_infos: [WhirlpoolRewardInfo {
                mint: Pubkey::default(),
                vault: Pubkey::default(),
                authority: Pubkey::default(),
                emissions_per_second_x64: 0,
                growth_global_x64: 0,
            }; NUM_REWARDS],
        }
    }

    #[test]
    fn test_whirlpool_swap_a_to_b() {
        let pool = test_whirlpool();
        let result = pool.calculate_swap_a_to_b(
            1_000_000_000,
            100_000_000_000,
            100_000_000_000,
        );
        assert!(result.amount_out > 0);
        assert!(result.fee_amount > 0);
        assert!(result.amount_out < 100_000_000_000);
    }

    #[test]
    fn test_whirlpool_swap_b_to_a() {
        let pool = test_whirlpool();
        let result = pool.calculate_swap_b_to_a(
            1_000_000_000,
            100_000_000_000,
            100_000_000_000,
        );
        assert!(result.amount_out > 0);
        assert!(result.fee_amount > 0);
    }

    #[test]
    fn test_whirlpool_simple_fallback() {
        let out = Whirlpool::calculate_swap_simple(
            1_000_000_000,
            100_000_000_000,
            100_000_000_000,
            3000, // 0.3% fee
        );
        assert!(out > 0);
        assert!(out < 100_000_000_000);
    }

    #[test]
    fn test_whirlpool_zero_liquidity() {
        let mut pool = test_whirlpool();
        pool.liquidity = 0;
        let result = pool.calculate_swap_a_to_b(1_000_000_000, 100_000_000, 100_000_000);
        assert_eq!(result.amount_out, 0);
    }
}
