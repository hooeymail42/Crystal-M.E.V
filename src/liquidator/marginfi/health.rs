//! MarginFi account health computation and liquidation sizing.
//!
//! The math here is deliberately layout-independent: it operates on already
//! decoded native amounts, USD prices, and weights, so it can be unit-tested
//! without any on-chain fixture. Callers assemble [`WeightedPosition`]s from
//! decoded [`super::account::Balance`]s + [`super::account::Bank`]s + oracle
//! prices, then evaluate health here.

/// One weighted position (a single balance valued in USD).
#[derive(Debug, Clone)]
pub struct WeightedPosition {
    /// Native token amount (already scaled out of shares).
    pub amount: f64,
    /// USD price per whole token.
    pub price_usd: f64,
    /// Token decimals (to convert native amount → whole tokens).
    pub decimals: u8,
    /// Maintenance weight applied to this side (asset or liability).
    pub weight_maint: f64,
    /// True if this is a liability (debt), false if an asset (collateral).
    pub is_liability: bool,
}

impl WeightedPosition {
    /// USD value of the whole-token amount (unweighted).
    pub fn usd_value(&self) -> f64 {
        let whole = self.amount / 10f64.powi(self.decimals as i32);
        whole * self.price_usd
    }

    /// Weighted USD value used in the health equation.
    pub fn weighted_usd(&self) -> f64 {
        self.usd_value() * self.weight_maint
    }
}

/// Result of a health evaluation for an account.
#[derive(Debug, Clone)]
pub struct HealthReport {
    /// Σ weighted asset USD.
    pub weighted_assets_usd: f64,
    /// Σ weighted liability USD.
    pub weighted_liabilities_usd: f64,
    /// Unweighted total collateral USD (used for sizing bonuses).
    pub total_assets_usd: f64,
    /// Unweighted total debt USD.
    pub total_liabilities_usd: f64,
}

impl HealthReport {
    /// Maintenance health: assets − liabilities in weighted USD.
    /// Negative ⇒ the account is liquidatable.
    pub fn maintenance_health(&self) -> f64 {
        self.weighted_assets_usd - self.weighted_liabilities_usd
    }

    /// Health factor in \[0, ∞): weighted_assets / weighted_liabilities.
    /// < 1.0 ⇒ liquidatable. Returns `f64::INFINITY` when there is no debt.
    pub fn health_factor(&self) -> f64 {
        if self.weighted_liabilities_usd <= 0.0 {
            f64::INFINITY
        } else {
            self.weighted_assets_usd / self.weighted_liabilities_usd
        }
    }

    /// Liquidatable when maintenance health has gone negative, i.e. weighted
    /// collateral no longer covers weighted debt. `buffer` (>= 0) tightens the
    /// trigger: only act when health is below `-buffer * liabilities`.
    pub fn is_liquidatable(&self, buffer: f64) -> bool {
        self.weighted_liabilities_usd > 0.0
            && self.maintenance_health() < -(buffer * self.weighted_liabilities_usd)
    }
}

/// Compute a [`HealthReport`] from all weighted positions of an account.
pub fn evaluate(positions: &[WeightedPosition]) -> HealthReport {
    let mut r = HealthReport {
        weighted_assets_usd: 0.0,
        weighted_liabilities_usd: 0.0,
        total_assets_usd: 0.0,
        total_liabilities_usd: 0.0,
    };
    for p in positions {
        if p.is_liability {
            r.weighted_liabilities_usd += p.weighted_usd();
            r.total_liabilities_usd += p.usd_value();
        } else {
            r.weighted_assets_usd += p.weighted_usd();
            r.total_assets_usd += p.usd_value();
        }
    }
    r
}

/// Estimate the maximum repay (in USD of the liability) allowed for a single
/// liquidation call.
///
/// MarginFi caps a liquidation so that it brings the account back toward
/// health without over-seizing. This conservative estimator returns the USD
/// value of debt that, when repaid, restores weighted health to ~0. The engine
/// further clamps this to available collateral, the flash-loan size, and DEX
/// depth. `liab_weight` and `asset_weight` are the maintenance weights of the
/// liability and seized-collateral banks respectively.
pub fn max_repay_usd(
    report: &HealthReport,
    liab_weight: f64,
    asset_weight: f64,
    liquidation_bonus: f64,
) -> f64 {
    let deficit = -report.maintenance_health();
    if deficit <= 0.0 {
        return 0.0;
    }
    // Repaying `x` USD of debt removes `x * liab_weight` weighted liability and
    // seizes `x * (1 + bonus)` collateral, removing `x*(1+bonus)*asset_weight`
    // weighted assets. Net health change per USD repaid:
    //   d(health)/dx = liab_weight - (1 + bonus) * asset_weight
    let per_usd = liab_weight - (1.0 + liquidation_bonus) * asset_weight;
    if per_usd <= 0.0 {
        // Liquidating this pair does not improve health; fall back to a fraction
        // of outstanding debt so the call still makes progress.
        return report.total_liabilities_usd * 0.5;
    }
    (deficit / per_usd).min(report.total_liabilities_usd)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(usd: f64, weight: f64) -> WeightedPosition {
        WeightedPosition {
            amount: usd,
            price_usd: 1.0,
            decimals: 0,
            weight_maint: weight,
            is_liability: false,
        }
    }
    fn liab(usd: f64, weight: f64) -> WeightedPosition {
        WeightedPosition {
            amount: usd,
            price_usd: 1.0,
            decimals: 0,
            weight_maint: weight,
            is_liability: true,
        }
    }

    #[test]
    fn healthy_account_not_liquidatable() {
        // 1000 collateral @0.8 weight = 800; 500 debt @1.0 weight = 500.
        let r = evaluate(&[asset(1000.0, 0.8), liab(500.0, 1.0)]);
        assert!(r.maintenance_health() > 0.0);
        assert!(r.health_factor() > 1.0);
        assert!(!r.is_liquidatable(0.0));
    }

    #[test]
    fn underwater_account_is_liquidatable() {
        // Collateral price crashed: 1000 collateral @0.8 = 800; 900 debt @1.0 = 900.
        let r = evaluate(&[asset(1000.0, 0.8), liab(900.0, 1.0)]);
        assert!(r.maintenance_health() < 0.0);
        assert!(r.health_factor() < 1.0);
        assert!(r.is_liquidatable(0.0));
    }

    #[test]
    fn buffer_prevents_marginal_liquidation() {
        // Slightly underwater: health = 800 - 810 = -10, liabilities = 810.
        let r = evaluate(&[asset(1000.0, 0.8), liab(810.0, 1.0)]);
        assert!(r.is_liquidatable(0.0));
        // A 5% buffer requires health < -40.5, so this is skipped.
        assert!(!r.is_liquidatable(0.05));
    }

    #[test]
    fn no_debt_is_infinite_health() {
        let r = evaluate(&[asset(1000.0, 0.8)]);
        assert!(r.health_factor().is_infinite());
        assert!(!r.is_liquidatable(0.0));
    }

    #[test]
    fn max_repay_positive_when_underwater() {
        let r = evaluate(&[asset(1000.0, 0.8), liab(900.0, 1.0)]);
        // liab_weight 1.0, asset_weight 0.8, 5% bonus.
        let repay = max_repay_usd(&r, 1.0, 0.8, 0.05);
        assert!(repay > 0.0);
        assert!(repay <= r.total_liabilities_usd);
    }

    #[test]
    fn max_repay_zero_when_healthy() {
        let r = evaluate(&[asset(1000.0, 0.8), liab(500.0, 1.0)]);
        assert_eq!(max_repay_usd(&r, 1.0, 0.8, 0.05), 0.0);
    }
}
