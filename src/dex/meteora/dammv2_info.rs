use anyhow::{Result, anyhow};
use solana_sdk::pubkey::Pubkey;

/// Meteora Dynamic AMM V2 pool state
#[derive(Debug, Clone)]
pub struct MeteoraDAmmV2Info {
    pub pool: Pubkey,
    pub token_a_mint: Pubkey,
    pub token_b_mint: Pubkey,
    pub a_vault: Pubkey,
    pub b_vault: Pubkey,
    pub a_vault_lp: Pubkey,
    pub b_vault_lp: Pubkey,
    pub a_vault_lp_bump: u8,
    pub enabled: bool,
    pub protocol_token_a_fee: u64,
    pub protocol_token_b_fee: u64,
    pub trade_fee_bps: u64,
    pub reserve_a: u64,
    pub reserve_b: u64,
}

impl MeteoraDAmmV2Info {
    pub fn try_deserialize(data: &[u8]) -> Result<Self> {
        if data.len() < 8 + 32 * 7 + 1 + 1 + 8 * 2 {
            return Err(anyhow!("Data too short for MeteoraDAmmV2Info: {} bytes", data.len()));
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

        let pool = read_pubkey!();
        let token_a_mint = read_pubkey!();
        let token_b_mint = read_pubkey!();
        let a_vault = read_pubkey!();
        let b_vault = read_pubkey!();
        let a_vault_lp = read_pubkey!();
        let b_vault_lp = read_pubkey!();
        let a_vault_lp_bump = d[offset]; offset += 1;
        let enabled = d[offset] != 0; offset += 1;
        let protocol_token_a_fee = u64::from_le_bytes(d[offset..offset+8].try_into().unwrap()); offset += 8;
        let protocol_token_b_fee = u64::from_le_bytes(d[offset..offset+8].try_into().unwrap()); offset += 8;

        Ok(Self {
            pool,
            token_a_mint,
            token_b_mint,
            a_vault,
            b_vault,
            a_vault_lp,
            b_vault_lp,
            a_vault_lp_bump,
            enabled,
            protocol_token_a_fee,
            protocol_token_b_fee,
            trade_fee_bps: 30, // default 0.3%
            reserve_a: 0, // set from vault balances
            reserve_b: 0,
        })
    }

    /// Constant product swap with 0.3% default fee
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_damm_v2_swap() {
        let out = MeteoraDAmmV2Info::calculate_swap_output(
            1_000_000_000,
            500_000_000_000,
            500_000_000_000,
            30, // 0.3% fee
        );
        assert!(out > 0);
        assert!(out < 500_000_000_000);
    }

    #[test]
    fn test_damm_v2_fee_impact() {
        let low_fee = MeteoraDAmmV2Info::calculate_swap_output(1_000_000, 100_000_000, 100_000_000, 10);
        let high_fee = MeteoraDAmmV2Info::calculate_swap_output(1_000_000, 100_000_000, 100_000_000, 100);
        assert!(low_fee > high_fee);
    }
}
