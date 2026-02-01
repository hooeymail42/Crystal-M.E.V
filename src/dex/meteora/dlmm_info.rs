use anyhow::{Result, anyhow};
use solana_sdk::pubkey::Pubkey;

/// Meteora DLMM (Dynamic Liquidity Market Maker) pool state
#[derive(Debug, Clone)]
pub struct MeteoraDlmmInfo {
    pub parameters: DlmmParameters,
    pub v_parameters: DlmmVParameters,
    pub bump_seed: [u8; 1],
    pub bin_step_seed: [u8; 2],
    pub pair_type: u8,
    pub active_id: i32,
    pub bin_step: u16,
    pub status: u8,
    pub require_base_factor_seed: u8,
    pub base_factor_seed: [u8; 2],
    pub token_x_mint: Pubkey,
    pub token_y_mint: Pubkey,
    pub reserve_x: Pubkey,
    pub reserve_y: Pubkey,
    pub protocol_fee_x: u64,
    pub protocol_fee_y: u64,
    pub reserve_x_amount: u64,
    pub reserve_y_amount: u64,
}

#[derive(Debug, Clone, Default)]
pub struct DlmmParameters {
    pub base_factor: u16,
    pub filter_period: u16,
    pub decay_period: u16,
    pub reduction_factor: u16,
    pub variable_fee_control: u32,
    pub protocol_share: u16,
    pub max_volatility_accumulator: u32,
    pub min_bin_id: i32,
    pub max_bin_id: i32,
}

#[derive(Debug, Clone, Default)]
pub struct DlmmVParameters {
    pub volatility_accumulator: u32,
    pub volatility_reference: u32,
    pub id_reference: i32,
    pub time_of_last_update: u64,
}

/// A single bin with its liquidity amounts
#[derive(Debug, Clone, Default)]
pub struct Bin {
    pub id: i32,
    pub amount_x: u64,
    pub amount_y: u64,
    pub price: f64,
}

/// DLMM bin-based swap calculator
#[derive(Debug, Clone)]
pub struct DlmmSwapCalculator {
    pub active_id: i32,
    pub bin_step: u16,
    pub base_factor: u16,
    pub variable_fee_control: u32,
    pub volatility_accumulator: u32,
    pub bins: Vec<Bin>,
}

impl DlmmSwapCalculator {
    /// Get the price for a given bin ID.
    /// Price = (1 + bin_step/10000) ^ (id - 0)
    /// Using the DLMM formula: price(id) = (1 + bin_step/10000)^id
    pub fn get_bin_price(bin_step: u16, bin_id: i32) -> f64 {
        let step = 1.0 + (bin_step as f64) / 10_000.0;
        step.powi(bin_id)
    }

    /// Calculate total fee rate in BPS (base fee + variable fee)
    pub fn get_total_fee_bps(&self) -> u64 {
        let base_fee_bps = (self.bin_step as u64) * (self.base_factor as u64) / 10_000;
        let base_fee_bps = base_fee_bps.max(1);

        // Variable fee: variable_fee_control * volatility_accumulator^2 / 10^15
        let variable_fee_bps = if self.variable_fee_control > 0 && self.volatility_accumulator > 0 {
            let va = self.volatility_accumulator as u128;
            let vfc = self.variable_fee_control as u128;
            // Scale: (vfc * va * va) / (10^15)
            // This gives BPS-scaled variable fee
            let raw = vfc * va * va;
            (raw / 1_000_000_000_000_000u128) as u64
        } else {
            0
        };

        (base_fee_bps + variable_fee_bps).min(1000) // cap at 10%
    }

    /// Swap X tokens for Y tokens (selling token X / buying token Y)
    /// Traverses bins from active_id downward
    pub fn swap_x_to_y(&self, amount_x_in: u64) -> SwapResult {
        let fee_bps = self.get_total_fee_bps();
        let fee_amount = (amount_x_in as u128 * fee_bps as u128 / 10_000) as u64;
        let amount_after_fee = amount_x_in.saturating_sub(fee_amount);

        let mut remaining_x = amount_after_fee as u128;
        let mut total_y_out: u128 = 0;
        let mut bins_crossed: u32 = 0;

        // Find active bin index in our bins vec
        let mut current_bin_idx = self.bins.iter().position(|b| b.id == self.active_id);

        while remaining_x > 0 {
            let idx = match current_bin_idx {
                Some(i) => i,
                None => break,
            };

            if idx >= self.bins.len() {
                break;
            }

            let bin = &self.bins[idx];
            if bin.amount_y == 0 {
                // No Y liquidity in this bin, move to next lower bin
                if idx == 0 { break; }
                current_bin_idx = Some(idx - 1);
                bins_crossed += 1;
                continue;
            }

            let bin_price = Self::get_bin_price(self.bin_step, bin.id);
            if bin_price <= 0.0 {
                break;
            }

            // How much X is needed to drain all Y from this bin?
            // x_needed = amount_y / price
            let y_available = bin.amount_y as u128;
            let x_needed = (y_available as f64 / bin_price) as u128;

            if remaining_x >= x_needed {
                // Consume entire bin's Y liquidity
                total_y_out += y_available;
                remaining_x -= x_needed;
                bins_crossed += 1;
                if idx == 0 { break; }
                current_bin_idx = Some(idx - 1);
            } else {
                // Partial fill in this bin
                let y_out = (remaining_x as f64 * bin_price) as u128;
                let y_out = y_out.min(y_available);
                total_y_out += y_out;
                remaining_x = 0;
            }
        }

        let amount_consumed = amount_after_fee as u128 - remaining_x;

        SwapResult {
            amount_in: amount_consumed as u64,
            amount_out: total_y_out as u64,
            fee_amount,
            bins_crossed,
        }
    }

    /// Swap Y tokens for X tokens (selling token Y / buying token X)
    /// Traverses bins from active_id upward
    pub fn swap_y_to_x(&self, amount_y_in: u64) -> SwapResult {
        let fee_bps = self.get_total_fee_bps();
        let fee_amount = (amount_y_in as u128 * fee_bps as u128 / 10_000) as u64;
        let amount_after_fee = amount_y_in.saturating_sub(fee_amount);

        let mut remaining_y = amount_after_fee as u128;
        let mut total_x_out: u128 = 0;
        let mut bins_crossed: u32 = 0;

        let mut current_bin_idx = self.bins.iter().position(|b| b.id == self.active_id);

        while remaining_y > 0 {
            let idx = match current_bin_idx {
                Some(i) => i,
                None => break,
            };

            if idx >= self.bins.len() {
                break;
            }

            let bin = &self.bins[idx];
            if bin.amount_x == 0 {
                if idx + 1 >= self.bins.len() { break; }
                current_bin_idx = Some(idx + 1);
                bins_crossed += 1;
                continue;
            }

            let bin_price = Self::get_bin_price(self.bin_step, bin.id);
            if bin_price <= 0.0 {
                break;
            }

            // How much Y is needed to drain all X from this bin?
            // y_needed = amount_x * price
            let x_available = bin.amount_x as u128;
            let y_needed = (x_available as f64 * bin_price) as u128;

            if remaining_y >= y_needed {
                total_x_out += x_available;
                remaining_y -= y_needed;
                bins_crossed += 1;
                if idx + 1 >= self.bins.len() { break; }
                current_bin_idx = Some(idx + 1);
            } else {
                let x_out = (remaining_y as f64 / bin_price) as u128;
                let x_out = x_out.min(x_available);
                total_x_out += x_out;
                remaining_y = 0;
            }
        }

        let amount_consumed = amount_after_fee as u128 - remaining_y;

        SwapResult {
            amount_in: amount_consumed as u64,
            amount_out: total_x_out as u64,
            fee_amount,
            bins_crossed,
        }
    }

    /// Convenience: create calculator from pool info with uniform liquidity distribution
    /// This is used when we don't have actual bin data (fallback approximation)
    pub fn from_pool_info_uniform(
        pool: &MeteoraDlmmInfo,
        reserve_x: u64,
        reserve_y: u64,
        num_bins: u32,
    ) -> Self {
        let half_bins = (num_bins / 2) as i32;
        let per_bin_x = reserve_x / num_bins as u64;
        let per_bin_y = reserve_y / num_bins as u64;

        let mut bins = Vec::with_capacity(num_bins as usize);
        for i in 0..num_bins as i32 {
            let bin_id = pool.active_id - half_bins + i;
            bins.push(Bin {
                id: bin_id,
                // X tokens above active bin, Y tokens below active bin
                amount_x: if bin_id >= pool.active_id { per_bin_x } else { 0 },
                amount_y: if bin_id <= pool.active_id { per_bin_y } else { 0 },
                price: Self::get_bin_price(pool.bin_step, bin_id),
            });
        }

        Self {
            active_id: pool.active_id,
            bin_step: pool.bin_step,
            base_factor: pool.parameters.base_factor,
            variable_fee_control: pool.parameters.variable_fee_control,
            volatility_accumulator: pool.v_parameters.volatility_accumulator,
            bins,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SwapResult {
    pub amount_in: u64,
    pub amount_out: u64,
    pub fee_amount: u64,
    pub bins_crossed: u32,
}

impl MeteoraDlmmInfo {
    pub fn try_deserialize(data: &[u8]) -> Result<Self> {
        if data.len() < 200 {
            return Err(anyhow!("Data too short for MeteoraDlmmInfo: {} bytes", data.len()));
        }

        let d = &data[8..]; // skip discriminator
        let mut offset = 0;

        // Parameters
        let base_factor = u16::from_le_bytes(d[offset..offset+2].try_into().unwrap()); offset += 2;
        let filter_period = u16::from_le_bytes(d[offset..offset+2].try_into().unwrap()); offset += 2;
        let decay_period = u16::from_le_bytes(d[offset..offset+2].try_into().unwrap()); offset += 2;
        let reduction_factor = u16::from_le_bytes(d[offset..offset+2].try_into().unwrap()); offset += 2;
        let variable_fee_control = u32::from_le_bytes(d[offset..offset+4].try_into().unwrap()); offset += 4;
        let protocol_share = u16::from_le_bytes(d[offset..offset+2].try_into().unwrap()); offset += 2;
        let max_volatility_accumulator = u32::from_le_bytes(d[offset..offset+4].try_into().unwrap()); offset += 4;
        let min_bin_id = i32::from_le_bytes(d[offset..offset+4].try_into().unwrap()); offset += 4;
        let max_bin_id = i32::from_le_bytes(d[offset..offset+4].try_into().unwrap()); offset += 4;

        let parameters = DlmmParameters {
            base_factor, filter_period, decay_period, reduction_factor,
            variable_fee_control, protocol_share, max_volatility_accumulator,
            min_bin_id, max_bin_id,
        };

        // VParameters
        let volatility_accumulator = u32::from_le_bytes(d[offset..offset+4].try_into().unwrap()); offset += 4;
        let volatility_reference = u32::from_le_bytes(d[offset..offset+4].try_into().unwrap()); offset += 4;
        let id_reference = i32::from_le_bytes(d[offset..offset+4].try_into().unwrap()); offset += 4;
        let time_of_last_update = u64::from_le_bytes(d[offset..offset+8].try_into().unwrap()); offset += 8;

        let v_parameters = DlmmVParameters {
            volatility_accumulator, volatility_reference, id_reference, time_of_last_update,
        };

        let mut bump_seed = [0u8; 1];
        bump_seed.copy_from_slice(&d[offset..offset+1]); offset += 1;
        let mut bin_step_seed = [0u8; 2];
        bin_step_seed.copy_from_slice(&d[offset..offset+2]); offset += 2;
        let pair_type = d[offset]; offset += 1;
        let active_id = i32::from_le_bytes(d[offset..offset+4].try_into().unwrap()); offset += 4;
        let bin_step = u16::from_le_bytes(d[offset..offset+2].try_into().unwrap()); offset += 2;
        let status = d[offset]; offset += 1;
        let require_base_factor_seed = d[offset]; offset += 1;
        let mut base_factor_seed = [0u8; 2];
        base_factor_seed.copy_from_slice(&d[offset..offset+2]); offset += 2;

        macro_rules! read_pubkey {
            () => {{
                let mut bytes = [0u8; 32];
                bytes.copy_from_slice(&d[offset..offset+32]);
                offset += 32;
                Pubkey::new_from_array(bytes)
            }};
        }

        let token_x_mint = read_pubkey!();
        let token_y_mint = read_pubkey!();
        let reserve_x = read_pubkey!();
        let reserve_y = read_pubkey!();

        let protocol_fee_x = u64::from_le_bytes(d[offset..offset+8].try_into().unwrap()); offset += 8;
        let _protocol_fee_y = u64::from_le_bytes(d[offset..offset+8].try_into().unwrap());

        let reserve_x_amount = 0;
        let reserve_y_amount = 0;

        Ok(Self {
            parameters,
            v_parameters,
            bump_seed,
            bin_step_seed,
            pair_type,
            active_id,
            bin_step,
            status,
            require_base_factor_seed,
            base_factor_seed,
            token_x_mint,
            token_y_mint,
            reserve_x,
            reserve_y,
            protocol_fee_x,
            protocol_fee_y: _protocol_fee_y,
            reserve_x_amount,
            reserve_y_amount,
        })
    }

    /// Calculate base fee in BPS from bin_step and base_factor
    pub fn get_base_fee_bps(&self) -> u64 {
        (self.bin_step as u64) * (self.parameters.base_factor as u64) / 10000
    }

    /// Bin-based swap: X to Y (sell token X for token Y)
    /// Falls back to uniform distribution if no bins provided
    pub fn calculate_swap_x_to_y(&self, amount_in: u64, reserve_x: u64, reserve_y: u64) -> SwapResult {
        let calc = DlmmSwapCalculator::from_pool_info_uniform(self, reserve_x, reserve_y, 20);
        calc.swap_x_to_y(amount_in)
    }

    /// Bin-based swap: Y to X (sell token Y for token X)
    pub fn calculate_swap_y_to_x(&self, amount_in: u64, reserve_x: u64, reserve_y: u64) -> SwapResult {
        let calc = DlmmSwapCalculator::from_pool_info_uniform(self, reserve_x, reserve_y, 20);
        calc.swap_y_to_x(amount_in)
    }

    /// Simplified constant-product fallback (kept for backward compatibility)
    pub fn calculate_swap_output(
        amount_in: u64,
        reserve_in: u64,
        reserve_out: u64,
        bin_step: u16,
        base_factor: u16,
    ) -> u64 {
        if reserve_in == 0 || reserve_out == 0 || amount_in == 0 {
            return 0;
        }

        let fee_bps = (bin_step as u64) * (base_factor as u64) / 10000;
        let fee_bps = fee_bps.max(1);

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

    fn test_pool() -> MeteoraDlmmInfo {
        MeteoraDlmmInfo {
            parameters: DlmmParameters {
                base_factor: 5000,
                filter_period: 30,
                decay_period: 120,
                reduction_factor: 5000,
                variable_fee_control: 40000,
                protocol_share: 1000,
                max_volatility_accumulator: 350000,
                min_bin_id: -100,
                max_bin_id: 100,
            },
            v_parameters: DlmmVParameters {
                volatility_accumulator: 10000,
                volatility_reference: 5000,
                id_reference: 0,
                time_of_last_update: 0,
            },
            bump_seed: [0],
            bin_step_seed: [0; 2],
            pair_type: 0,
            active_id: 0,
            bin_step: 20,
            status: 0,
            require_base_factor_seed: 0,
            base_factor_seed: [0; 2],
            token_x_mint: Pubkey::default(),
            token_y_mint: Pubkey::default(),
            reserve_x: Pubkey::default(),
            reserve_y: Pubkey::default(),
            protocol_fee_x: 0,
            protocol_fee_y: 0,
            reserve_x_amount: 100_000_000_000,
            reserve_y_amount: 100_000_000_000,
        }
    }

    #[test]
    fn test_bin_price() {
        // bin_step=20, id=0 => price = 1.0
        let price_0 = DlmmSwapCalculator::get_bin_price(20, 0);
        assert!((price_0 - 1.0).abs() < 0.0001);

        // bin_step=20, id=1 => price = 1.002
        let price_1 = DlmmSwapCalculator::get_bin_price(20, 1);
        assert!(price_1 > 1.0);
        assert!((price_1 - 1.002).abs() < 0.001);

        // bin_step=20, id=-1 => price < 1.0
        let price_neg = DlmmSwapCalculator::get_bin_price(20, -1);
        assert!(price_neg < 1.0);
    }

    #[test]
    fn test_total_fee_bps() {
        let calc = DlmmSwapCalculator {
            active_id: 0,
            bin_step: 20,
            base_factor: 5000,
            variable_fee_control: 40000,
            volatility_accumulator: 10000,
            bins: vec![],
        };
        let fee = calc.get_total_fee_bps();
        // base_fee = 20 * 5000 / 10000 = 10 bps
        assert!(fee >= 10);
    }

    #[test]
    fn test_swap_x_to_y_basic() {
        let pool = test_pool();
        let result = pool.calculate_swap_x_to_y(
            1_000_000_000,
            100_000_000_000,
            100_000_000_000,
        );
        assert!(result.amount_out > 0);
        assert!(result.fee_amount > 0);
    }

    #[test]
    fn test_swap_y_to_x_basic() {
        let pool = test_pool();
        let result = pool.calculate_swap_y_to_x(
            1_000_000_000,
            100_000_000_000,
            100_000_000_000,
        );
        assert!(result.amount_out > 0);
        assert!(result.fee_amount > 0);
    }

    #[test]
    fn test_swap_roundtrip_loses_value() {
        let pool = test_pool();
        let result_xy = pool.calculate_swap_x_to_y(
            1_000_000_000,
            100_000_000_000,
            100_000_000_000,
        );
        // Swap back
        let result_yx = pool.calculate_swap_y_to_x(
            result_xy.amount_out,
            100_000_000_000,
            100_000_000_000,
        );
        // Should lose value due to fees
        assert!(result_yx.amount_out < 1_000_000_000);
    }

    #[test]
    fn test_constant_product_fallback() {
        let out = MeteoraDlmmInfo::calculate_swap_output(
            1_000_000_000,
            500_000_000_000,
            500_000_000_000,
            10,
            100,
        );
        assert!(out > 0);
        assert!(out < 500_000_000_000);
    }

    #[test]
    fn test_dlmm_fee_calculation() {
        let low_fee = MeteoraDlmmInfo::calculate_swap_output(1_000_000, 100_000_000, 100_000_000, 10, 100);
        let high_fee = MeteoraDlmmInfo::calculate_swap_output(1_000_000, 100_000_000, 100_000_000, 100, 5000);
        assert!(low_fee > high_fee);
    }
}
