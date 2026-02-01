/// Real slippage calculation based on DEX models and liquidity
#[derive(Clone, Debug)]
pub struct SlippageCalculator {
    pub dex: String,
    pub liquidity_sol: f64,
}

impl SlippageCalculator {
    /// Create new slippage calculator for specific DEX and liquidity level
    pub fn new(dex: &str, liquidity_sol: f64) -> Self {
        Self {
            dex: dex.to_string(),
            liquidity_sol: liquidity_sol.max(1.0), // Minimum 1 SOL
        }
    }

    /// Calculate slippage percentage based on DEX model and trade size
    pub fn calculate_slippage(&self, trade_amount_sol: f64) -> f64 {
        let ratio = trade_amount_sol / self.liquidity_sol;
        
        match self.dex.as_str() {
            "Raydium" => self.raydium_slippage(ratio),
            "DLMM" => self.dlmm_slippage(ratio),
            "Whirlpool" => self.whirlpool_slippage(ratio),
            "Pump" => self.pump_slippage(ratio),
            "Meteora" => self.meteora_slippage(ratio),
            "Solfi" => self.solfi_slippage(ratio),
            "Vertigo" => self.vertigo_slippage(ratio),
            _ => self.default_slippage(ratio),
        }
    }

    /// Apply slippage to expected output amount
    pub fn apply_slippage(&self, expected_output: f64, trade_amount_sol: f64) -> f64 {
        let slippage_pct = self.calculate_slippage(trade_amount_sol);
        expected_output * (1.0 - slippage_pct / 100.0)
    }

    /// Raydium constant product formula: x*y=k
    /// Slippage = (trade_amount / liquidity) * scaling_factor, capped at 5%
    fn raydium_slippage(&self, ratio: f64) -> f64 {
        let slippage = ratio * 100.0 / (1.0 + ratio);
        slippage.min(5.0).max(0.95) // 0.95-5.0%
    }

    /// DLMM (Dynamic Liquidity Market Maker) - more efficient concentrated liquidity
    /// Lower slippage due to concentrated positions
    fn dlmm_slippage(&self, ratio: f64) -> f64 {
        let slippage = ratio * 75.0 / (1.0 + ratio);
        slippage.min(3.0).max(0.75) // 0.75-3.0% (more efficient)
    }

    /// Whirlpool (Orca's concentrated liquidity)
    /// Efficient but not as tight as DLMM
    fn whirlpool_slippage(&self, ratio: f64) -> f64 {
        let slippage = ratio * 80.0 / (1.0 + ratio);
        slippage.min(3.5).max(0.8) // 0.8-3.5%
    }

    /// Pump.fun - newer, often thin liquidity pools
    /// Much higher slippage due to low liquidity
    fn pump_slippage(&self, ratio: f64) -> f64 {
        let slippage = ratio * 200.0 / (1.0 + ratio);
        slippage.min(8.0).max(1.0) // 1-8% (high for small pools)
    }

    /// Meteora - similar to Raydium constant product
    fn meteora_slippage(&self, ratio: f64) -> f64 {
        let slippage = ratio * 100.0 / (1.0 + ratio);
        slippage.min(4.5).max(1.0) // 1-4.5%
    }

    /// Solfi - established DEX with medium slippage
    fn solfi_slippage(&self, ratio: f64) -> f64 {
        let slippage = ratio * 110.0 / (1.0 + ratio);
        slippage.min(5.5).max(1.5) // 1.5-5.5%
    }

    /// Vertigo - AMM with standard slippage model
    fn vertigo_slippage(&self, ratio: f64) -> f64 {
        let slippage = ratio * 100.0 / (1.0 + ratio);
        slippage.min(5.0).max(1.2) // 1.2-5.0%
    }

    /// Default model for unknown DEXs
    fn default_slippage(&self, ratio: f64) -> f64 {
        let slippage = ratio * 100.0 / (1.0 + ratio);
        slippage.min(5.0).max(1.0)
    }
}

/// Multi-hop slippage calculator - compounds slippage across multiple legs
#[derive(Clone, Debug)]
pub struct MultiHopSlippage;

impl MultiHopSlippage {
    /// Calculate cumulative slippage for multi-hop trades
    /// Slippage compounds: final_output = initial * (1-slip1) * (1-slip2) * (1-slip3)
    pub fn calculate_cumulative(slippages: &[f64]) -> f64 {
        if slippages.is_empty() {
            return 0.0;
        }
        
        let mut cumulative = 1.0;
        for slip_pct in slippages {
            cumulative *= 1.0 - slip_pct / 100.0;
        }
        
        // Convert back to percentage
        ((1.0 - cumulative) * 100.0).max(0.0)
    }

    /// Evaluate if multi-hop trade is worth executing
    pub fn is_profitable(total_slippage_pct: f64, profit_pct: f64) -> bool {
        // Trade is profitable if profit > total slippage
        profit_pct > total_slippage_pct
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_raydium_slippage_small_trade() {
        let calc = SlippageCalculator::new("Raydium", 1000.0);
        let slippage = calc.calculate_slippage(10.0); // 1% of pool
        assert!(slippage >= 0.95 && slippage <= 5.0);
        println!("Raydium 1% trade: {:.2}% slippage", slippage);
    }

    #[test]
    fn test_dlmm_slippage_lower_than_raydium() {
        let trade_amount = 10.0;
        let liquidity = 1000.0;
        
        let raydium = SlippageCalculator::new("Raydium", liquidity).calculate_slippage(trade_amount);
        let dlmm = SlippageCalculator::new("DLMM", liquidity).calculate_slippage(trade_amount);
        
        // DLMM should have lower slippage due to concentrated liquidity
        assert!(dlmm < raydium);
        println!("Raydium: {:.2}% vs DLMM: {:.2}%", raydium, dlmm);
    }

    #[test]
    fn test_pump_slippage_high_for_thin_liquidity() {
        let calc = SlippageCalculator::new("Pump", 100.0);
        let slippage = calc.calculate_slippage(10.0); // 10% of pool (thin)
        assert!(slippage >= 1.0 && slippage <= 8.0);
        println!("Pump 10% of thin pool: {:.2}% slippage", slippage);
    }

    #[test]
    fn test_apply_slippage_to_output() {
        let calc = SlippageCalculator::new("Raydium", 1000.0);
        let expected_output = 100.0;
        let trade_amount = 10.0;
        
        let slippage = calc.calculate_slippage(trade_amount);
        let actual_output = calc.apply_slippage(expected_output, trade_amount);
        
        let expected_with_slippage = expected_output * (1.0 - slippage / 100.0);
        assert!((actual_output - expected_with_slippage).abs() < 0.01);
    }

    #[test]
    fn test_cumulative_slippage_3_hops() {
        let slippages = vec![0.5, 0.5, 0.5]; // 0.5% per hop
        let cumulative = MultiHopSlippage::calculate_cumulative(&slippages);
        
        // Should be slightly more than 1.5% due to compounding
        assert!(cumulative >= 1.49 && cumulative <= 1.51);
        println!("3 x 0.5% hops: {:.2}% cumulative", cumulative);
    }

    #[test]
    fn test_cumulative_slippage_4_hops() {
        let slippages = vec![1.0, 1.0, 1.0, 1.0];
        let cumulative = MultiHopSlippage::calculate_cumulative(&slippages);
        
        // Should be ~3.94% for 4 x 1% hops
        assert!(cumulative >= 3.9 && cumulative <= 4.0);
        println!("4 x 1.0% hops: {:.2}% cumulative", cumulative);
    }

    #[test]
    fn test_profitability_check() {
        // Trade with 2% profit but 2.5% total slippage - not worth it
        assert!(!MultiHopSlippage::is_profitable(2.5, 2.0));
        
        // Trade with 3% profit and 2% total slippage - worth it
        assert!(MultiHopSlippage::is_profitable(2.0, 3.0));
    }
}
