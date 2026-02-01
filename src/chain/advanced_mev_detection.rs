use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use chrono::Utc;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MevOpportunity {
    pub opportunity_type: String,
    pub pool_a: Pubkey,
    pub pool_b: Pubkey,
    pub estimated_profit: f64,
    pub confidence: f64,
    pub urgency: f64,
    pub timestamp: i64,
}

#[derive(Clone, Debug)]
pub struct AdvancedMevDetector {
    min_profit_threshold: f64,
    min_confidence: f64,
    sandwich_detection_enabled: bool,
}

impl AdvancedMevDetector {
    pub fn new(min_profit: f64) -> Self {
        Self {
            min_profit_threshold: min_profit,
            min_confidence: 0.75,
            sandwich_detection_enabled: true,
        }
    }

    pub fn detect_arbitrage(&self, pool_a_price: f64, pool_b_price: f64, liquidity: f64) -> Result<Option<MevOpportunity>> {
        let spread = (pool_a_price - pool_b_price).abs();
        let spread_pct = (spread / pool_a_price.max(pool_b_price)) * 100.0;

        if spread_pct < 0.5 {
            return Ok(None);
        }

        let estimated_profit = spread_pct * liquidity * 0.001;
        let confidence = (spread_pct / 5.0).min(1.0);

        if estimated_profit >= self.min_profit_threshold && confidence >= self.min_confidence {
            Ok(Some(MevOpportunity {
                opportunity_type: "Arbitrage".to_string(),
                pool_a: Pubkey::default(),
                pool_b: Pubkey::default(),
                estimated_profit,
                confidence,
                urgency: spread_pct / 5.0,
                timestamp: Utc::now().timestamp(),
            }))
        } else {
            Ok(None)
        }
    }

    pub fn detect_sandwich_attack(&self, pending_trades: usize, pool_liquidity: f64) -> Result<f64> {
        if !self.sandwich_detection_enabled || pending_trades == 0 {
            return Ok(0.0);
        }

        let sandwich_risk = (pending_trades as f64 / pool_liquidity.max(1.0)) * 100.0;
        Ok(sandwich_risk.min(100.0))
    }

    pub fn detect_flash_loan(&self, transaction_size: f64, pool_reserves: f64) -> Result<bool> {
        let flash_loan_threshold = pool_reserves * 10.0;
        Ok(transaction_size > flash_loan_threshold)
    }

    pub fn score_opportunity(&self, profit: f64, risk: f64, speed: f64) -> f64 {
        let profit_score = (profit / 10.0).min(1.0);
        let risk_score = 1.0 - (risk / 100.0).min(1.0);
        let speed_score = speed / 1000.0;

        (profit_score * 0.5 + risk_score * 0.3 + speed_score * 0.2).min(1.0)
    }

    pub fn rank_opportunities(&self, opportunities: Vec<MevOpportunity>) -> Vec<MevOpportunity> {
        let mut ranked = opportunities;
        ranked.sort_by(|a, b| {
            let score_a = self.score_opportunity(a.estimated_profit, 0.0, a.urgency);
            let score_b = self.score_opportunity(b.estimated_profit, 0.0, b.urgency);
            score_b.partial_cmp(&score_a).unwrap_or(std::cmp::Ordering::Equal)
        });
        ranked
    }

    pub fn analyze_mempool(&self, pending_count: usize) -> String {
        if pending_count > 100 {
            "High competition - expect increased slippage".to_string()
        } else if pending_count > 50 {
            "Moderate competition - proceed with caution".to_string()
        } else {
            "Low competition - favorable conditions".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_arbitrage_detection() {
        let detector = AdvancedMevDetector::new(0.1);
        let result = detector.detect_arbitrage(100.0, 95.0, 1000.0).unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn test_sandwich_risk() {
        let detector = AdvancedMevDetector::new(0.1);
        let risk = detector.detect_sandwich_attack(5, 50000.0).unwrap();
        assert!(risk > 0.0);
    }

    #[test]
    fn test_flash_loan_detection() {
        let detector = AdvancedMevDetector::new(0.1);
        let is_flash = detector.detect_flash_loan(1000000.0, 50000.0).unwrap();
        assert!(is_flash);
    }
}
