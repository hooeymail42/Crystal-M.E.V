#![allow(dead_code)]
/// Gas fee awareness for MEV bot profitability filtering
use std::collections::HashMap;

/// Lamport costs for different operations (empirically calibrated)
#[derive(Clone, Debug)]
pub struct GasCosts {
    /// Single swap cost (base gas)
    pub swap_cost: u64,
    /// Cost per additional hop
    pub hop_cost: u64,
    /// Token wrapping/unwrapping cost
    pub transfer_cost: u64,
    /// Base priority fee
    pub base_cost: u64,
}

impl Default for GasCosts {
    fn default() -> Self {
        Self {
            swap_cost: 5000,      // 5K lamports per swap
            hop_cost: 8000,       // 8K lamports per hop
            transfer_cost: 2500,  // 2.5K lamports per transfer
            base_cost: 5000,      // 5K lamports base
        }
    }
}

/// Configuration for gas-based trade filtering
#[derive(Clone, Debug)]
pub struct GasFeeConfig {
    /// Minimum profit in SOL required to execute (aggressive: 0.005 for high confidence)
    pub min_profit_to_execute_sol: f64,
    /// Safety margin as percentage (aggressive: 10% for faster execution)
    pub safety_margin_percent: f64,
    /// DEX-specific priority fee multipliers (higher for fast execution)
    pub dex_fee_multipliers: HashMap<String, f64>,
    /// Enable aggressive execution on high-confidence trades (>90% confidence)
    pub aggressive_mode: bool,
}

impl Default for GasFeeConfig {
    fn default() -> Self {
        let mut multipliers = HashMap::new();
        multipliers.insert("Raydium".to_string(), 1.0);    // Base fee
        multipliers.insert("Pump".to_string(), 1.2);       // Higher priority for volatile pools
        multipliers.insert("DLMM".to_string(), 0.9);       // Slightly lower for liquid pools
        multipliers.insert("Whirlpool".to_string(), 0.95);
        multipliers.insert("Meteora".to_string(), 1.1);
        multipliers.insert("Solfi".to_string(), 1.15);
        multipliers.insert("Vertigo".to_string(), 1.1);
        multipliers.insert("Phoenix".to_string(), 0.85);
        multipliers.insert("Lifinity".to_string(), 0.95);
        multipliers.insert("Heaven".to_string(), 1.3);

        Self {
            min_profit_to_execute_sol: 0.0001,      // Default: require at least 0.1 mSOL profit (overridden by MIN_PROFIT_SOL env var)
            safety_margin_percent: 0.10,           // OPTIMIZED: 10% margin (down from 20%)
            dex_fee_multipliers: multipliers,
            aggressive_mode: true,                  // OPTIMIZED: Enable aggressive execution
        }
    }
}

impl GasFeeConfig {
    /// Calculate total gas cost for a trade path
    pub fn calculate_total(&self, num_hops: usize, needs_wrap: bool, needs_unwrap: bool) -> u64 {
        let base = self.swap_cost();
        let hop_costs = (num_hops as u64 - 1) * self.hop_cost();
        let wrap_cost = if needs_wrap { self.transfer_cost() } else { 0 };
        let unwrap_cost = if needs_unwrap { self.transfer_cost() } else { 0 };
        
        base + hop_costs + wrap_cost + unwrap_cost
    }

    /// Get swap cost (base operation cost)
    pub fn swap_cost(&self) -> u64 {
        GasCosts::default().swap_cost
    }

    /// Get hop cost for multi-leg trades
    pub fn hop_cost(&self) -> u64 {
        GasCosts::default().hop_cost
    }

    /// Get transfer cost (wrapping/unwrapping)
    pub fn transfer_cost(&self) -> u64 {
        GasCosts::default().transfer_cost
    }

    /// Check if profit is sufficient to execute trade
    pub fn should_execute(&self, gross_profit_sol: f64, num_hops: usize) -> bool {
        let breakeven = self.breakeven_profit(num_hops);
        
        // Aggressive mode: execute if profit > minimum (no safety margin required)
        if self.aggressive_mode && gross_profit_sol >= self.min_profit_to_execute_sol {
            return true;
        }
        
        // Normal mode: require safety margin
        gross_profit_sol >= breakeven
    }

    /// Calculate minimum profit needed to cover gas costs with safety margin
    pub fn breakeven_profit(&self, num_hops: usize) -> f64 {
        let gas_lamports = self.calculate_total(num_hops, false, false);
        let gas_sol = gas_lamports as f64 / 1_000_000_000.0;
        
        // Apply safety margin
        gas_sol * (1.0 + self.safety_margin_percent)
    }

    /// Get DEX-specific fee multiplier
    pub fn get_dex_multiplier(&self, dex: &str) -> f64 {
        *self.dex_fee_multipliers.get(dex).unwrap_or(&1.0)
    }

    /// Estimate execution cost in SOL with DEX multiplier
    pub fn estimate_cost_sol(&self, dex: &str, num_hops: usize) -> f64 {
        let base_lamports = self.calculate_total(num_hops, false, false) as f64;
        let multiplier = self.get_dex_multiplier(dex);
        (base_lamports * multiplier) / 1_000_000_000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gas_calculation_2_hop() {
        let config = GasFeeConfig::default();
        let costs = GasCosts::default();
        
        // 2-hop: swap + 1 hop cost
        let total = config.calculate_total(2, false, false);
        assert_eq!(total, costs.swap_cost + costs.hop_cost);
    }

    #[test]
    fn test_gas_calculation_3_hop() {
        let config = GasFeeConfig::default();
        let costs = GasCosts::default();
        
        // 3-hop: swap + 2 hop costs
        let total = config.calculate_total(3, false, false);
        assert_eq!(total, costs.swap_cost + (2 * costs.hop_cost));
    }

    #[test]
    fn test_profitability_check_aggressive_mode() {
        let config = GasFeeConfig::default();
        assert!(config.aggressive_mode); // Verify aggressive is enabled
        
        // Even small profits should execute in aggressive mode
        assert!(config.should_execute(0.005, 2)); // 0.5 cents
        assert!(config.should_execute(0.01, 2));  // 1 cent
    }

    #[test]
    fn test_dex_multipliers() {
        let config = GasFeeConfig::default();
        assert_eq!(config.get_dex_multiplier("Raydium"), 1.0);
        assert_eq!(config.get_dex_multiplier("Pump"), 1.2);  // Higher for volatile
        assert_eq!(config.get_dex_multiplier("DLMM"), 0.9);  // Lower for liquid
    }

    #[test]
    fn test_estimate_cost_with_multiplier() {
        let config = GasFeeConfig::default();
        
        let pump_cost = config.estimate_cost_sol("Pump", 2);
        let raydium_cost = config.estimate_cost_sol("Raydium", 2);
        
        // Pump should be more expensive (higher priority fee)
        assert!(pump_cost > raydium_cost);
    }
}
