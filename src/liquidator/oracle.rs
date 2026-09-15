//! Minimal price-oracle reading for collateral/liability valuation.
//!
//! MarginFi banks price positions with Pyth or Switchboard feeds. Using a
//! wrong, stale, or low-confidence price is a direct way to lose money on a
//! liquidation, so [`OraclePrice`] carries confidence + staleness and the engine
//! rejects prices that fail [`OraclePrice::is_usable`].
//!
//! Only the Pyth price-account layout is decoded here (the common case). For
//! Switchboard feeds, plug a parser into [`parse_price`] following the same
//! shape. **VERIFY the Pyth offsets against a live feed before real execution.**

use anyhow::{anyhow, bail, Result};

/// A normalized oracle price in USD.
#[derive(Debug, Clone, Copy)]
pub struct OraclePrice {
    pub price_usd: f64,
    /// Confidence interval in USD (1σ).
    pub conf_usd: f64,
    /// Publish slot / timestamp of the price (for staleness checks).
    pub publish_time: i64,
}

impl OraclePrice {
    /// Reject unusable prices: non-positive, wide confidence, or stale.
    /// `max_conf_pct` e.g. 0.02 = 2% max confidence-to-price ratio.
    /// `max_age_secs` bounds staleness against `now_unix`.
    pub fn is_usable(&self, max_conf_pct: f64, max_age_secs: i64, now_unix: i64) -> bool {
        if self.price_usd <= 0.0 {
            return false;
        }
        if self.conf_usd / self.price_usd > max_conf_pct {
            return false;
        }
        if now_unix - self.publish_time > max_age_secs {
            return false;
        }
        true
    }
}

/// Pyth price-account field offsets (v2 price account). **VERIFY.**
mod pyth {
    pub const MAGIC: usize = 0; // u32 0xa1b2c3d4
    pub const EXPO: usize = 20; // i32
    pub const AGG_PRICE: usize = 208; // i64
    pub const AGG_CONF: usize = 216; // u64
    pub const TIMESTAMP: usize = 224; // i64 (publish time)
    pub const MAGIC_VALUE: u32 = 0xa1b2c3d4;
}

fn read_i32(d: &[u8], o: usize) -> Result<i32> {
    Ok(i32::from_le_bytes(
        d.get(o..o + 4)
            .ok_or_else(|| anyhow!("i32 oob at {}", o))?
            .try_into()
            .unwrap(),
    ))
}
fn read_i64(d: &[u8], o: usize) -> Result<i64> {
    Ok(i64::from_le_bytes(
        d.get(o..o + 8)
            .ok_or_else(|| anyhow!("i64 oob at {}", o))?
            .try_into()
            .unwrap(),
    ))
}
fn read_u32(d: &[u8], o: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        d.get(o..o + 4)
            .ok_or_else(|| anyhow!("u32 oob at {}", o))?
            .try_into()
            .unwrap(),
    ))
}

/// Parse a Pyth price account into a normalized USD price.
pub fn parse_pyth_price(data: &[u8]) -> Result<OraclePrice> {
    if data.len() < pyth::TIMESTAMP + 8 {
        bail!("pyth account too short: {} bytes", data.len());
    }
    if read_u32(data, pyth::MAGIC)? != pyth::MAGIC_VALUE {
        bail!("not a pyth price account (bad magic)");
    }
    let expo = read_i32(data, pyth::EXPO)?;
    let raw_price = read_i64(data, pyth::AGG_PRICE)?;
    let raw_conf = read_i64(data, pyth::AGG_CONF)? as i64;
    let publish_time = read_i64(data, pyth::TIMESTAMP)?;

    let scale = 10f64.powi(expo); // expo is typically negative
    Ok(OraclePrice {
        price_usd: raw_price as f64 * scale,
        conf_usd: (raw_conf as f64).abs() * scale,
        publish_time,
    })
}

/// Dispatch on oracle account type. Currently Pyth only; extend for Switchboard.
pub fn parse_price(data: &[u8]) -> Result<OraclePrice> {
    parse_pyth_price(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usability_gate() {
        let now = 1_000_000i64;
        let good = OraclePrice {
            price_usd: 100.0,
            conf_usd: 0.5,
            publish_time: now - 5,
        };
        assert!(good.is_usable(0.02, 60, now));

        let wide = OraclePrice {
            price_usd: 100.0,
            conf_usd: 5.0,
            publish_time: now,
        };
        assert!(!wide.is_usable(0.02, 60, now));

        let stale = OraclePrice {
            price_usd: 100.0,
            conf_usd: 0.1,
            publish_time: now - 600,
        };
        assert!(!stale.is_usable(0.02, 60, now));

        let zero = OraclePrice {
            price_usd: 0.0,
            conf_usd: 0.0,
            publish_time: now,
        };
        assert!(!zero.is_usable(0.02, 60, now));
    }

    #[test]
    fn rejects_non_pyth() {
        assert!(parse_pyth_price(&[0u8; 300]).is_err());
    }
}
