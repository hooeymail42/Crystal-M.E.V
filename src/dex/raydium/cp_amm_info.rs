use anyhow::{Result, anyhow};
use solana_sdk::pubkey::Pubkey;

/// Raydium CP-AMM (Constant Product) pool state
#[derive(Debug, Clone)]
pub struct RaydiumCpAmmInfo {
    pub amm_config: Pubkey,
    pub pool_creator: Pubkey,
    pub token_0_vault: Pubkey,
    pub token_1_vault: Pubkey,
    pub lp_mint: Pubkey,
    pub token_0_mint: Pubkey,
    pub token_1_mint: Pubkey,
    pub token_0_program: Pubkey,
    pub token_1_program: Pubkey,
    pub observation_key: Pubkey,
    pub auth_bump: u8,
    pub status: u8,
    pub lp_mint_decimals: u8,
    pub mint_0_decimals: u8,
    pub mint_1_decimals: u8,
    pub lp_supply: u64,
    pub protocol_fees_token_0: u64,
    pub protocol_fees_token_1: u64,
    pub fund_fees_token_0: u64,
    pub fund_fees_token_1: u64,
    pub open_time: u64,
}

impl RaydiumCpAmmInfo {
    pub fn try_deserialize(data: &[u8]) -> Result<Self> {
        if data.len() < 8 + 32 * 10 + 5 + 8 * 5 {
            return Err(anyhow!("Data too short for RaydiumCpAmmInfo: {} bytes", data.len()));
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

        macro_rules! read_u8 {
            () => {{
                let val = d[offset];
                offset += 1;
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

        Ok(Self {
            amm_config: read_pubkey!(),
            pool_creator: read_pubkey!(),
            token_0_vault: read_pubkey!(),
            token_1_vault: read_pubkey!(),
            lp_mint: read_pubkey!(),
            token_0_mint: read_pubkey!(),
            token_1_mint: read_pubkey!(),
            token_0_program: read_pubkey!(),
            token_1_program: read_pubkey!(),
            observation_key: read_pubkey!(),
            auth_bump: read_u8!(),
            status: read_u8!(),
            lp_mint_decimals: read_u8!(),
            mint_0_decimals: read_u8!(),
            mint_1_decimals: read_u8!(),
            lp_supply: read_u64!(),
            protocol_fees_token_0: read_u64!(),
            protocol_fees_token_1: read_u64!(),
            fund_fees_token_0: read_u64!(),
            fund_fees_token_1: read_u64!(),
            open_time: read_u64!(),
        })
    }

    /// Constant product swap with 0.25% fee (same math as AMM V4)
    pub fn calculate_swap_output(
        amount_in: u64,
        reserve_in: u64,
        reserve_out: u64,
    ) -> u64 {
        if reserve_in == 0 || reserve_out == 0 || amount_in == 0 {
            return 0;
        }
        // 0.25% fee
        let amount_in_with_fee = (amount_in as u128) * 9975;
        let numerator = amount_in_with_fee * (reserve_out as u128);
        let denominator = (reserve_in as u128) * 10000 + amount_in_with_fee;
        if denominator == 0 { return 0; }
        (numerator / denominator) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cp_amm_swap() {
        let out = RaydiumCpAmmInfo::calculate_swap_output(
            1_000_000_000,
            1000_000_000_000,
            500_000_000_000,
        );
        assert!(out > 0);
        assert!(out < 500_000_000_000);
    }

    #[test]
    fn test_cp_amm_swap_symmetry() {
        // Swapping A->B then B->A should lose value (due to fees)
        let reserve = 100_000_000_000u64;
        let amount = 1_000_000_000u64;
        let out1 = RaydiumCpAmmInfo::calculate_swap_output(amount, reserve, reserve);
        let out2 = RaydiumCpAmmInfo::calculate_swap_output(out1, reserve - out1, reserve + amount);
        assert!(out2 < amount); // Lost to fees
    }
}
