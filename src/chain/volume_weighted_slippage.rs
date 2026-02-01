use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use anyhow::{Result, anyhow};

/// Historical volume data for a pool
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VolumeHistory {
    pub pool: Pubkey,
    pub hourly_volumes: Vec<f64>,
    pub daily_volume: f64,
    pub weekly_volume: f64,
    pub avg_trade_size: f64,
    pub trades_per_hour: u32,
    pub last_updated: i64,
    pub volatility_score: f64,
}

impl VolumeHistory {
    pub fn new(pool: Pubkey) -> Self {
        Self {
            pool,
            hourly_volumes: Vec::new(),
            daily_volume: 0.0,
            weekly_volume: 0.0,
            avg_trade_size: 0.0,
            trades_per_hour: 0,
            last_updated: Utc::now().timestamp(),
            volatility_score: 0.0,
        }
    }

    /// Add hourly volume data
    pub fn add_hourly_volume(&mut self, volume: f64) {
        self.hourly_volumes.push(volume);
        
        // Keep only last 7 days (168 hours)
        if self.hourly_volumes.len() > 168 {
            self.hourly_volumes.remove(0);
        }

        self.calculate_metrics();
    }

    /// Calculate derived metrics
    fn calculate_metrics(&mut self) {
        if self.hourly_volumes.is_empty() {
            return;
        }

        // Last 24 hours = last 24 elements
        let last_24_idx = self.hourly_volumes.len().saturating_sub(24);
        self.daily_volume = self.hourly_volumes[last_24_idx..].iter().sum();

        // Last 7 days = all elements
        self.weekly_volume = self.hourly_volumes.iter().sum();

        // Average trade size (estimated from volume)
        let avg_volume_per_hour = self.daily_volume / 24.0;
        self.avg_trade_size = (avg_volume_per_hour / self.trades_per_hour.max(1) as f64).max(10.0);

        // Volatility = standard deviation of hourly volumes
        self.calculate_volatility();
    }

    fn calculate_volatility(&mut self) {
        if self.hourly_volumes.len() < 2 {
            self.volatility_score = 0.0;
            return;
        }

        let mean = self.hourly_volumes.iter().sum::<f64>() / self.hourly_volumes.len() as f64;
        let variance = self.hourly_volumes
            .iter()
            .map(|v| (v - mean).powi(2))
            .sum::<f64>() / self.hourly_volumes.len() as f64;

        self.volatility_score = variance.sqrt() / mean.max(1.0);
    }

    /// Get liquidity/volume ratio (higher = more stable)
    pub fn get_stability_score(&self) -> f64 {
        // Normalize volatility to 0-1 range
        (1.0 / (1.0 + self.volatility_score)).min(1.0)
    }
}

/// Volume-weighted slippage predictor
pub struct VolumeWeightedSlippagePredictor {
    volume_history: HashMap<Pubkey, VolumeHistory>,
    dex: String,
}

impl VolumeWeightedSlippagePredictor {
    pub fn new(dex: String) -> Self {
        Self {
            volume_history: HashMap::new(),
            dex,
        }
    }

    /// Add pool to tracking
    pub fn track_pool(&mut self, pool: Pubkey) {
        self.volume_history.insert(pool, VolumeHistory::new(pool));
    }

    /// Update volume for a pool
    pub fn update_volume(&mut self, pool: Pubkey, volume: f64) -> Result<()> {
        self.volume_history
            .entry(pool)
            .or_insert_with(|| VolumeHistory::new(pool))
            .add_hourly_volume(volume);

        Ok(())
    }

    /// Predict slippage based on volume history and trade size
    pub fn predict_slippage(
        &self,
        pool: Pubkey,
        trade_size_sol: f64,
        liquidity_sol: f64,
    ) -> Result<f64> {
        let history = self.volume_history.get(&pool)
            .ok_or_else(|| anyhow!("No volume history for pool: {}", pool))?;

        // Base slippage formula
        let impact_ratio = trade_size_sol / liquidity_sol.max(1.0);

        // Volume-adjusted slippage
        let volume_factor = (history.daily_volume / history.avg_trade_size.max(1.0)).min(10.0);
        let volatility_factor = 1.0 + (history.volatility_score * 0.5); // Add volatility premium

        // DEX-specific multiplier
        let dex_multiplier = self.get_dex_multiplier();

        // Final slippage calculation
        let base_slippage = impact_ratio * 100.0; // Percentage
        let adjusted = base_slippage * dex_multiplier / volume_factor * volatility_factor;

        // Clamp to reasonable bounds
        let slippage = adjusted.max(0.05).min(15.0); // 0.05% - 15%

        Ok(slippage)
    }

    /// Predict multi-hop slippage with compounding
    pub fn predict_multi_hop_slippage(
        &self,
        pools: &[Pubkey],
        trade_size_sol: f64,
        liquidity_values: &[f64],
    ) -> Result<f64> {
        if pools.is_empty() {
            return Ok(0.0);
        }

        let mut cumulative_slippage = 1.0;

        for (i, pool) in pools.iter().enumerate() {
            let slippage_percent = self.predict_slippage(
                *pool,
                trade_size_sol,
                liquidity_values.get(i).copied().unwrap_or(100.0),
            )?;

            // Compound slippage: (1 - slip1) * (1 - slip2) * (1 - slip3)
            cumulative_slippage *= 1.0 - slippage_percent / 100.0;
        }

        // Convert back to percentage
        let total_slippage_percent = (1.0 - cumulative_slippage) * 100.0;
        Ok(total_slippage_percent)
    }

    /// Get slippage impact on profit
    pub fn calculate_profit_after_slippage(
        &self,
        pool: Pubkey,
        gross_profit_sol: f64,
        trade_size_sol: f64,
        liquidity_sol: f64,
    ) -> Result<f64> {
        let slippage_percent = self.predict_slippage(pool, trade_size_sol, liquidity_sol)?;
        let slippage_amount = trade_size_sol * (slippage_percent / 100.0);

        let net_profit = gross_profit_sol - slippage_amount;
        Ok(net_profit.max(0.0))
    }

    /// DEX-specific multiplier
    fn get_dex_multiplier(&self) -> f64 {
        match self.dex.as_str() {
            "DLMM" => 0.8,      // Most efficient
            "Raydium" => 1.0,   // Standard
            "Whirlpool" => 1.1, // Slightly less efficient
            "Pump" => 1.4,      // Thin liquidity
            "Meteora" => 1.15,
            "Solfi" => 1.25,
            "Vertigo" => 1.2,
            _ => 1.0,
        }
    }

    /// Get slippage trend (increasing/decreasing/stable)
    pub fn get_slippage_trend(&self, pool: Pubkey) -> Result<SlippageTrend> {
        let history = self.volume_history.get(&pool)
            .ok_or_else(|| anyhow!("No volume history for pool"))?;

        if history.hourly_volumes.len() < 2 {
            return Ok(SlippageTrend::Stable);
        }

        let recent_volumes = &history.hourly_volumes[history.hourly_volumes.len().saturating_sub(6)..];
        let recent_avg = recent_volumes.iter().sum::<f64>() / recent_volumes.len() as f64;
        let older_volumes = &history.hourly_volumes[history.hourly_volumes.len().saturating_sub(12)..history.hourly_volumes.len().saturating_sub(6)];
        let older_avg = older_volumes.iter().sum::<f64>() / older_volumes.len() as f64;

        let change = (recent_avg - older_avg) / older_avg.max(1.0);

        let trend = if change > 0.1 {
            SlippageTrend::Decreasing // More volume = lower slippage
        } else if change < -0.1 {
            SlippageTrend::Increasing // Less volume = higher slippage
        } else {
            SlippageTrend::Stable
        };

        Ok(trend)
    }

    /// Recommend optimal trade size based on slippage
    pub fn recommend_trade_size(
        &self,
        pool: Pubkey,
        max_slippage_percent: f64,
        liquidity_sol: f64,
    ) -> Result<f64> {
        // Binary search for optimal size
        let mut low = 0.1;
        let mut high = liquidity_sol * 0.5; // Never trade more than 50% of pool

        for _ in 0..20 {
            let mid = (low + high) / 2.0;
            let slippage = self.predict_slippage(pool, mid, liquidity_sol)?;

            if slippage > max_slippage_percent {
                high = mid;
            } else {
                low = mid;
            }
        }

        Ok(low)
    }

    /// Export volume history to CSV
    pub fn export_volume_history(&self, filename: &str) -> Result<()> {
        use std::fs::File;
        use std::io::Write;

        let mut file = File::create(filename)?;
        writeln!(file, "pool,daily_volume,weekly_volume,volatility,stability_score,avg_trade_size")?;

        for history in self.volume_history.values() {
            writeln!(
                file,
                "{},{:.2},{:.2},{:.4},{:.4},{:.2}",
                history.pool,
                history.daily_volume,
                history.weekly_volume,
                history.volatility_score,
                history.get_stability_score(),
                history.avg_trade_size
            )?;
        }

        println!("💾 Exported volume history to {}", filename);
        Ok(())
    }

    /// Get statistics summary
    pub fn get_summary(&self) -> VolumeSummary {
        let total_pools = self.volume_history.len();
        let total_daily_volume: f64 = self.volume_history.values()
            .map(|h| h.daily_volume)
            .sum();
        let avg_volatility = self.volume_history.values()
            .map(|h| h.volatility_score)
            .sum::<f64>() / total_pools.max(1) as f64;

        VolumeSummary {
            total_pools,
            total_daily_volume,
            avg_volatility,
            dex: self.dex.clone(),
        }
    }

    /// Print summary
    pub fn print_summary(&self) {
        let summary = self.get_summary();
        println!("\n📊 Volume-Weighted Slippage Summary");
        println!("├─ DEX: {}", summary.dex);
        println!("├─ Total Pools: {}", summary.total_pools);
        println!("├─ Daily Volume: ${:.2}", summary.total_daily_volume);
        println!("└─ Avg Volatility: {:.4}", summary.avg_volatility);
    }
}

#[derive(Debug, Clone)]
pub enum SlippageTrend {
    Increasing,
    Decreasing,
    Stable,
}

#[derive(Debug, Clone)]
pub struct VolumeSummary {
    pub total_pools: usize,
    pub total_daily_volume: f64,
    pub avg_volatility: f64,
    pub dex: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_volume_history() {
        let pool = Pubkey::default();
        let mut history = VolumeHistory::new(pool);

        history.add_hourly_volume(1000.0);
        history.add_hourly_volume(1200.0);
        history.add_hourly_volume(900.0);

        assert!(history.daily_volume > 0.0);
        assert!(history.volatility_score >= 0.0);
    }

    #[test]
    fn test_slippage_prediction() {
        let pool = Pubkey::default();
        let mut predictor = VolumeWeightedSlippagePredictor::new("Raydium".to_string());

        predictor.track_pool(pool);
        predictor.update_volume(pool, 10000.0).unwrap();

        // Predict slippage for a trade
        let slippage = predictor.predict_slippage(pool, 100.0, 5000.0).unwrap();
        assert!(slippage > 0.0);
        assert!(slippage < 15.0);
    }

    #[test]
    fn test_multi_hop_slippage() {
        let pools = vec![Pubkey::default(), Pubkey::default()];
        let mut predictor = VolumeWeightedSlippagePredictor::new("Raydium".to_string());

        for pool in &pools {
            predictor.track_pool(*pool);
            predictor.update_volume(*pool, 5000.0).unwrap();
        }

        let slippage = predictor.predict_multi_hop_slippage(
            &pools,
            100.0,
            &[5000.0, 5000.0],
        ).unwrap();

        assert!(slippage > 0.0);
        assert!(slippage < 15.0);
    }

    #[test]
    fn test_stability_score() {
        let pool = Pubkey::default();
        let mut history = VolumeHistory::new(pool);

        // Add consistent volumes
        for _ in 0..10 {
            history.add_hourly_volume(1000.0);
        }

        let stability = history.get_stability_score();
        assert!(stability > 0.8); // High stability
    }
}
