//! AI/ML engine stubs — lightweight persistence-backed learning layer.
//! Tracks trade outcomes, scores opportunities, and adapts thresholds over time.

use crate::chain::opportunity_detector::OpportunityConfig;
use serde::{Deserialize, Serialize};
use tracing::info;

// ── TradeRecord ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeRecord {
    pub timestamp_utc: i64,
    pub hour_utc: u8,
    pub dex_a: String,
    pub dex_b: String,
    pub token_mint: String,
    pub input_sol: f64,
    pub output_sol: f64,
    pub profit_pct: f64,
    pub ai_score: f64,
    pub submitted: bool,
    pub confirmed: bool,
    pub profitable: bool,
}

// ── TradeMemory ───────────────────────────────────────────────────────────────

const MEMORY_PATH: &str = "trade_memory.json";

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct TradeMemory {
    pub records: Vec<TradeRecord>,
}

impl TradeMemory {
    pub fn load() -> Self {
        std::fs::read_to_string(MEMORY_PATH)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(MEMORY_PATH, json);
        }
    }

    pub fn record(&mut self, record: TradeRecord) {
        self.records.push(record);
        // Keep last 10 000 records to cap memory usage.
        if self.records.len() > 10_000 {
            self.records.drain(0..1_000);
        }
    }

    /// Win rate for a specific DEX pair over the last 200 trades.
    pub fn pair_win_rate(&self, dex_a: &str, dex_b: &str) -> f64 {
        let relevant: Vec<_> = self
            .records
            .iter()
            .rev()
            .take(200)
            .filter(|r| r.dex_a == dex_a && r.dex_b == dex_b && r.confirmed)
            .collect();
        if relevant.is_empty() {
            return 0.5; // neutral prior
        }
        let wins = relevant.iter().filter(|r| r.profitable).count();
        wins as f64 / relevant.len() as f64
    }

    /// Win rate for a specific UTC hour over the last 500 trades.
    pub fn hour_win_rate(&self, hour: u8) -> f64 {
        let relevant: Vec<_> = self
            .records
            .iter()
            .rev()
            .take(500)
            .filter(|r| r.hour_utc == hour && r.confirmed)
            .collect();
        if relevant.is_empty() {
            return 0.5;
        }
        let wins = relevant.iter().filter(|r| r.profitable).count();
        wins as f64 / relevant.len() as f64
    }

    /// Overall win rate across all confirmed trades.
    pub fn overall_win_rate(&self) -> f64 {
        let confirmed: Vec<_> = self.records.iter().filter(|r| r.confirmed).collect();
        if confirmed.is_empty() {
            return 0.5;
        }
        let wins = confirmed.iter().filter(|r| r.profitable).count();
        wins as f64 / confirmed.len() as f64
    }

    pub fn print_summary(&self) {
        let confirmed = self.records.iter().filter(|r| r.confirmed).count();
        let profitable = self.records.iter().filter(|r| r.profitable).count();
        info!(
            "[TradeMemory] total={} confirmed={} profitable={} win_rate={:.1}%",
            self.records.len(),
            confirmed,
            profitable,
            self.overall_win_rate() * 100.0
        );
    }
}

// ── ScoringFeatures ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ScoringFeatures {
    pub profit_pct_norm: f64,
    pub liquidity_norm: f64,
    pub dex_a_rep: f64,
    pub dex_b_rep: f64,
    pub pair_history: f64,
    pub hour_score: f64,
    pub congestion_score: f64,
}

impl ScoringFeatures {
    /// Static reputation score for a DEX name (0.0–1.0).
    pub fn dex_reputation(dex: &str) -> f64 {
        match dex {
            "raydium" | "raydium_v4" | "raydium_cp" => 0.95,
            "whirlpool" | "orca" => 0.90,
            "meteora_dlmm" | "meteora" => 0.85,
            "pump" | "pump_amm" => 0.70,
            _ => 0.60,
        }
    }

    fn weighted_score(&self) -> f64 {
        self.profit_pct_norm * 0.30
            + self.liquidity_norm * 0.20
            + self.dex_a_rep * 0.10
            + self.dex_b_rep * 0.10
            + self.pair_history * 0.15
            + self.hour_score * 0.10
            + (1.0 - self.congestion_score) * 0.05
    }
}

// ── OpportunityScorer ─────────────────────────────────────────────────────────

const SCORER_PATH: &str = "opportunity_scorer.json";

#[derive(Debug, Serialize, Deserialize)]
pub struct OpportunityScorer {
    pub threshold: f64,
    pub total_updates: usize,
    // Exponential moving average of weighted score for profitable vs unprofitable trades.
    ema_profitable: f64,
    ema_unprofitable: f64,
}

impl Default for OpportunityScorer {
    fn default() -> Self {
        Self {
            threshold: 0.45,
            total_updates: 0,
            ema_profitable: 0.60,
            ema_unprofitable: 0.35,
        }
    }
}

impl OpportunityScorer {
    pub fn load() -> Self {
        std::fs::read_to_string(SCORER_PATH)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(SCORER_PATH, json);
        }
    }

    /// Returns (passes, score). Skips filter until 30 updates have been recorded.
    pub fn passes(&self, features: &ScoringFeatures) -> (bool, f64) {
        let score = features.weighted_score();
        (score >= self.threshold, score)
    }

    /// Update internal model from trade outcome.
    pub fn update(&mut self, features: &ScoringFeatures, profitable: bool, _profit_pct: f64) {
        let score = features.weighted_score();
        let alpha = 0.05;
        if profitable {
            self.ema_profitable = alpha * score + (1.0 - alpha) * self.ema_profitable;
        } else {
            self.ema_unprofitable = alpha * score + (1.0 - alpha) * self.ema_unprofitable;
        }
        self.total_updates += 1;
    }

    /// Adjust threshold based on overall win rate.
    pub fn adapt_threshold(&mut self, win_rate: f64) {
        // Raise bar if we're winning too often (leave profit on table), lower if losing too much.
        let target = (self.ema_profitable + self.ema_unprofitable) / 2.0;
        let adjustment = (win_rate - 0.55) * 0.02; // ±2% per cycle
        self.threshold = (target + adjustment).clamp(0.20, 0.80);
    }

    pub fn print_status(&self) {
        info!(
            "[Scorer] threshold={:.3} updates={} ema_win={:.3} ema_loss={:.3}",
            self.threshold, self.total_updates, self.ema_profitable, self.ema_unprofitable
        );
    }
}

// ── MarketAnalyzer ────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct MarketAnalyzer {
    loop_durations_ms: Vec<f64>,
    price_ratios: Vec<f64>,
}

impl MarketAnalyzer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_loop_duration_ms(&mut self, ms: f64) {
        self.loop_durations_ms.push(ms);
        if self.loop_durations_ms.len() > 1_000 {
            self.loop_durations_ms.drain(0..200);
        }
    }

    pub fn record_price_ratio(&mut self, ratio: f64) {
        self.price_ratios.push(ratio);
        if self.price_ratios.len() > 1_000 {
            self.price_ratios.drain(0..200);
        }
    }

    /// Network congestion proxy: 1.0 = very congested, 0.0 = clear.
    pub fn congestion_score(&self) -> f64 {
        if self.loop_durations_ms.is_empty() {
            return 0.3;
        }
        let recent: Vec<f64> = self.loop_durations_ms.iter().rev().take(20).copied().collect();
        let avg_ms = recent.iter().sum::<f64>() / recent.len() as f64;
        // 200 ms loop = clear; 2000 ms loop = congested.
        (avg_ms / 2000.0).clamp(0.0, 1.0)
    }

    pub fn print_status(&self) {
        let avg_loop = if self.loop_durations_ms.is_empty() {
            0.0
        } else {
            self.loop_durations_ms.iter().rev().take(50).sum::<f64>()
                / self.loop_durations_ms.len().min(50) as f64
        };
        info!(
            "[MarketAnalyzer] avg_loop_ms={:.0} congestion={:.2} price_samples={}",
            avg_loop,
            self.congestion_score(),
            self.price_ratios.len()
        );
    }
}

// ── AdaptiveParams ────────────────────────────────────────────────────────────

const ADAPTIVE_PATH: &str = "adaptive_params.json";

#[derive(Debug, Serialize, Deserialize)]
pub struct AdaptiveParams {
    pub base_min_profit_pct: f64,
    pub base_max_trade_sol: f64,
    pub adaptation_count: usize,
    min_liquidity_sol: f64,
    max_slippage_pct: f64,
}

impl Default for AdaptiveParams {
    fn default() -> Self {
        Self {
            base_min_profit_pct: 0.3,
            base_max_trade_sol: 5.0,
            adaptation_count: 0,
            min_liquidity_sol: 1.0,
            max_slippage_pct: 1.0,
        }
    }
}

impl AdaptiveParams {
    pub fn load() -> Self {
        std::fs::read_to_string(ADAPTIVE_PATH)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(ADAPTIVE_PATH, json);
        }
    }

    pub fn to_opportunity_config(&self) -> OpportunityConfig {
        OpportunityConfig {
            min_profit_percent: self.base_min_profit_pct,
            min_liquidity_sol: self.min_liquidity_sol,
            max_slippage_percent: self.max_slippage_pct,
            max_volatility_percent: 10.0,
        }
    }

    /// Adapt thresholds based on recent win rate and market conditions.
    pub fn adapt(&mut self, win_rate: f64, market: &MarketAnalyzer) {
        let congestion = market.congestion_score();

        // Raise min profit bar when congested or losing.
        if win_rate < 0.40 || congestion > 0.70 {
            self.base_min_profit_pct = (self.base_min_profit_pct * 1.05).min(2.0);
        } else if win_rate > 0.65 && congestion < 0.30 {
            // Lower bar slightly when conditions are favourable.
            self.base_min_profit_pct = (self.base_min_profit_pct * 0.97).max(0.1);
        }

        self.adaptation_count += 1;
        self.save();

        info!(
            "[AdaptiveParams] cycle={} win_rate={:.1}% congestion={:.2} min_profit={:.3}%",
            self.adaptation_count,
            win_rate * 100.0,
            congestion,
            self.base_min_profit_pct
        );
    }
}
