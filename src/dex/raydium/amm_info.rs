#![allow(dead_code)]
use anyhow::{Result, anyhow};
use solana_sdk::pubkey::Pubkey;

/// Raydium AMM V4 pool state (on-chain layout after 8-byte discriminator)
#[derive(Debug, Clone)]
pub struct RaydiumAmmInfo {
    pub status: u64,
    pub nonce: u64,
    pub order_num: u64,
    pub depth: u64,
    pub coin_decimals: u64,
    pub pc_decimals: u64,
    pub state: u64,
    pub reset_flag: u64,
    pub min_size: u64,
    pub vol_max_cut_ratio: u64,
    pub amount_wave_ratio: u64,
    pub coin_lot_size: u64,
    pub pc_lot_size: u64,
    pub min_price_multiplier: u64,
    pub max_price_multiplier: u64,
    pub sys_decimal_value: u64,
    pub min_separate_numerator: u64,
    pub min_separate_denominator: u64,
    pub trade_fee_numerator: u64,
    pub trade_fee_denominator: u64,
    pub pnl_numerator: u64,
    pub pnl_denominator: u64,
    pub swap_fee_numerator: u64,
    pub swap_fee_denominator: u64,
    pub need_take_pnl_coin: u64,
    pub need_take_pnl_pc: u64,
    pub total_pnl_pc: u64,
    pub total_pnl_coin: u64,
    pub pool_open_time: u64,
    pub punish_pc_amount: u64,
    pub punish_coin_amount: u64,
    pub orderbook_to_init_time: u64,
    pub swap_coin_in_amount: u128,
    pub swap_pc_out_amount: u128,
    pub swap_coin2_pc_fee: u64,
    pub swap_pc_in_amount: u128,
    pub swap_coin_out_amount: u128,
    pub swap_pc2_coin_fee: u64,
    pub pool_coin_token_account: Pubkey,
    pub pool_pc_token_account: Pubkey,
    pub coin_mint_address: Pubkey,
    pub pc_mint_address: Pubkey,
    pub lp_mint_address: Pubkey,
    pub amm_open_orders: Pubkey,
    pub serum_market: Pubkey,
    pub serum_program_id: Pubkey,
    pub amm_target_orders: Pubkey,
    pub pool_withdraw_queue: Pubkey,
    pub pool_temp_lp_token_account: Pubkey,
    pub amm_owner: Pubkey,
    pub pnl_owner: Pubkey,
}

impl RaydiumAmmInfo {
    #[allow(unused_assignments)]
    pub fn try_deserialize(data: &[u8]) -> Result<Self> {
        if data.len() < 752 {
            return Err(anyhow!("Data too short for RaydiumAmmInfo: {} bytes", data.len()));
        }

        // Raydium V4 AMM is NOT Anchor-based — it has NO 8-byte discriminator.
        // The first byte is the start of the struct (status: u64).
        let d = data;
        let mut offset = 0;

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

        macro_rules! read_pubkey {
            () => {{
                let mut bytes = [0u8; 32];
                bytes.copy_from_slice(&d[offset..offset+32]);
                offset += 32;
                Pubkey::new_from_array(bytes)
            }};
        }

        Ok(Self {
            status: read_u64!(),
            nonce: read_u64!(),
            order_num: read_u64!(),
            depth: read_u64!(),
            coin_decimals: read_u64!(),
            pc_decimals: read_u64!(),
            state: read_u64!(),
            reset_flag: read_u64!(),
            min_size: read_u64!(),
            vol_max_cut_ratio: read_u64!(),
            amount_wave_ratio: read_u64!(),
            coin_lot_size: read_u64!(),
            pc_lot_size: read_u64!(),
            min_price_multiplier: read_u64!(),
            max_price_multiplier: read_u64!(),
            sys_decimal_value: read_u64!(),
            min_separate_numerator: read_u64!(),
            min_separate_denominator: read_u64!(),
            trade_fee_numerator: read_u64!(),
            trade_fee_denominator: read_u64!(),
            pnl_numerator: read_u64!(),
            pnl_denominator: read_u64!(),
            swap_fee_numerator: read_u64!(),
            swap_fee_denominator: read_u64!(),
            need_take_pnl_coin: read_u64!(),
            need_take_pnl_pc: read_u64!(),
            total_pnl_pc: read_u64!(),
            total_pnl_coin: read_u64!(),
            pool_open_time: read_u64!(),
            punish_pc_amount: read_u64!(),
            punish_coin_amount: read_u64!(),
            orderbook_to_init_time: read_u64!(),
            swap_coin_in_amount: read_u128!(),
            swap_pc_out_amount: read_u128!(),
            swap_coin2_pc_fee: read_u64!(),
            swap_pc_in_amount: read_u128!(),
            swap_coin_out_amount: read_u128!(),
            swap_pc2_coin_fee: read_u64!(),
            pool_coin_token_account: read_pubkey!(),
            pool_pc_token_account: read_pubkey!(),
            coin_mint_address: read_pubkey!(),
            pc_mint_address: read_pubkey!(),
            lp_mint_address: read_pubkey!(),
            amm_open_orders: read_pubkey!(),
            serum_market: read_pubkey!(),
            serum_program_id: read_pubkey!(),
            amm_target_orders: read_pubkey!(),
            pool_withdraw_queue: read_pubkey!(),
            pool_temp_lp_token_account: read_pubkey!(),
            amm_owner: read_pubkey!(),
            pnl_owner: if d.len() >= offset + 32 { read_pubkey!() } else { Pubkey::default() },
        })
    }

    /// Constant product swap: amount_out = (amount_in * (1 - fee) * reserve_out) / (reserve_in + amount_in * (1 - fee))
    /// Default fee: 0.25% (numerator=25, denominator=10000)
    pub fn calculate_swap_output(
        &self,
        amount_in: u64,
        reserve_in: u64,
        reserve_out: u64,
    ) -> u64 {
        if reserve_in == 0 || reserve_out == 0 || amount_in == 0 {
            return 0;
        }

        let fee_num = self.trade_fee_numerator;
        let fee_den = self.trade_fee_denominator;

        // amount_in_with_fee = amount_in * (fee_den - fee_num)
        let amount_in_with_fee = (amount_in as u128) * ((fee_den - fee_num) as u128);
        let numerator = amount_in_with_fee * (reserve_out as u128);
        let denominator = (reserve_in as u128) * (fee_den as u128) + amount_in_with_fee;

        if denominator == 0 {
            return 0;
        }

        (numerator / denominator) as u64
    }
}

/// Calculate Raydium AMM V4 swap with default 0.25% fee
pub fn calculate_raydium_swap(amount_in: u64, reserve_in: u64, reserve_out: u64) -> u64 {
    if reserve_in == 0 || reserve_out == 0 || amount_in == 0 {
        return 0;
    }
    // 0.25% fee: fee_num=25, fee_den=10000
    let amount_in_with_fee = (amount_in as u128) * 9975;
    let numerator = amount_in_with_fee * (reserve_out as u128);
    let denominator = (reserve_in as u128) * 10000 + amount_in_with_fee;
    (numerator / denominator) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_raydium_swap_basic() {
        // 1 SOL in, reserves 1000 SOL / 1_000_000 tokens
        let amount_out = calculate_raydium_swap(
            1_000_000_000, // 1 SOL in lamports
            1000_000_000_000, // 1000 SOL reserve
            1_000_000_000_000, // 1M tokens reserve (6 decimals)
        );
        // Expected: ~997_506 tokens (constant product with 0.25% fee)
        assert!(amount_out > 0);
        assert!(amount_out < 1_000_000_000_000); // Must be less than total reserve
    }

    #[test]
    fn test_raydium_swap_zero_input() {
        assert_eq!(calculate_raydium_swap(0, 1000, 1000), 0);
    }

    #[test]
    fn test_raydium_swap_zero_reserves() {
        assert_eq!(calculate_raydium_swap(100, 0, 1000), 0);
        assert_eq!(calculate_raydium_swap(100, 1000, 0), 0);
    }

    #[test]
    fn test_raydium_swap_fee_applied() {
        // With fee, output should be less than no-fee constant product
        let with_fee = calculate_raydium_swap(1_000_000, 100_000_000, 100_000_000);
        // No fee: amount_out = 1M * 100M / (100M + 1M) = 990099
        let no_fee = (1_000_000u128 * 100_000_000u128 / (100_000_000u128 + 1_000_000u128)) as u64;
        assert!(with_fee < no_fee);
        assert!(with_fee > 0);
    }
}
