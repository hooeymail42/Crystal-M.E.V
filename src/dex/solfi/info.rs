use anyhow::{Result, anyhow};
use solana_sdk::pubkey::Pubkey;

/// SolFi pool state
#[derive(Debug, Clone)]
pub struct SolfiInfo {
    pub pool: Pubkey,
    pub token_a_mint: Pubkey,
    pub token_b_mint: Pubkey,
    pub token_a_vault: Pubkey,
    pub token_b_vault: Pubkey,
    pub fee_bps: u64,
    pub reserve_a: u64,
    pub reserve_b: u64,
}

impl SolfiInfo {
    pub fn try_deserialize(data: &[u8], pool_address: &Pubkey) -> Result<Self> {
        if data.len() < 8 + 32 * 4 + 8 {
            return Err(anyhow!("Data too short for SolfiInfo: {} bytes", data.len()));
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

        let token_a_mint = read_pubkey!();
        let token_b_mint = read_pubkey!();
        let token_a_vault = read_pubkey!();
        let token_b_vault = read_pubkey!();

        let fee_bps = if d.len() > offset + 8 {
            u64::from_le_bytes(d[offset..offset+8].try_into().unwrap_or([0u8; 8]))
        } else {
            30 // default 0.3%
        };

        Ok(Self {
            pool: *pool_address,
            token_a_mint,
            token_b_mint,
            token_a_vault,
            token_b_vault,
            fee_bps,
            reserve_a: 0,
            reserve_b: 0,
        })
    }

    /// Constant product swap with configurable fee
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
    fn test_solfi_swap_basic() {
        let out = SolfiInfo::calculate_swap_output(
            1_000_000_000,
            500_000_000_000,
            500_000_000_000,
            30,
        );
        assert!(out > 0);
        assert!(out < 500_000_000_000);
    }

    #[test]
    fn test_solfi_swap_fee_impact() {
        let low = SolfiInfo::calculate_swap_output(1_000_000, 100_000_000, 100_000_000, 10);
        let high = SolfiInfo::calculate_swap_output(1_000_000, 100_000_000, 100_000_000, 100);
        assert!(low > high);
    }

    #[test]
    fn test_solfi_swap_zero() {
        assert_eq!(SolfiInfo::calculate_swap_output(0, 100, 100, 30), 0);
        assert_eq!(SolfiInfo::calculate_swap_output(100, 0, 100, 30), 0);
    }

    #[test]
    fn test_solfi_roundtrip_loses() {
        let reserve = 100_000_000_000u64;
        let out1 = SolfiInfo::calculate_swap_output(1_000_000_000, reserve, reserve, 30);
        let out2 = SolfiInfo::calculate_swap_output(out1, reserve - out1, reserve + 1_000_000_000, 30);
        assert!(out2 < 1_000_000_000);
    }
}
