use anyhow::{Result, anyhow};
use solana_sdk::pubkey::Pubkey;

/// Phoenix orderbook market state
#[derive(Debug, Clone)]
pub struct PhoenixMarketState {
    pub discriminator: u64,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
    pub base_lot_size: u64,
    pub quote_lot_size: u64,
    pub tick_size_in_quote_lots_per_base_unit: u64,
    pub order_sequence_number: u64,
    pub taker_fee_bps: u16,
    pub best_bid_price: u64,
    pub best_bid_size: u64,
    pub best_ask_price: u64,
    pub best_ask_size: u64,
}

impl PhoenixMarketState {
    pub fn try_deserialize(data: &[u8]) -> Result<Self> {
        // 8 (disc) + 32*4 (mints/vaults) + 8*4 (lot sizes etc) + 2 (fee) + 8*4 (bid/ask)
        let min_len = 8 + 32 * 4 + 8 * 4 + 2 + 8 * 4;
        if data.len() < min_len {
            return Err(anyhow!("Data too short for PhoenixMarketState: {} bytes", data.len()));
        }

        let mut offset = 0;

        macro_rules! read_u64 {
            ($d:expr) => {{
                let val = u64::from_le_bytes($d[offset..offset+8].try_into().unwrap());
                offset += 8;
                val
            }};
        }

        macro_rules! read_pubkey {
            ($d:expr) => {{
                let mut bytes = [0u8; 32];
                bytes.copy_from_slice(&$d[offset..offset+32]);
                offset += 32;
                Pubkey::new_from_array(bytes)
            }};
        }

        let discriminator = read_u64!(data);
        let base_mint = read_pubkey!(data);
        let quote_mint = read_pubkey!(data);
        let base_vault = read_pubkey!(data);
        let quote_vault = read_pubkey!(data);
        let base_lot_size = read_u64!(data);
        let quote_lot_size = read_u64!(data);
        let tick_size_in_quote_lots_per_base_unit = read_u64!(data);
        let order_sequence_number = read_u64!(data);
        let taker_fee_bps = u16::from_le_bytes(data[offset..offset+2].try_into().unwrap());
        offset += 2;
        let best_bid_price = read_u64!(data);
        let best_bid_size = read_u64!(data);
        let best_ask_price = read_u64!(data);
        let best_ask_size = read_u64!(data);

        Ok(Self {
            discriminator,
            base_mint,
            quote_mint,
            base_vault,
            quote_vault,
            base_lot_size,
            quote_lot_size,
            tick_size_in_quote_lots_per_base_unit,
            order_sequence_number,
            taker_fee_bps,
            best_bid_price,
            best_bid_size,
            best_ask_price,
            best_ask_size,
        })
    }

    /// Buy base tokens with quote input, walking the asks.
    /// Returns base token output amount after taker fee.
    pub fn calculate_buy_output(&self, quote_amount_in: u64) -> u64 {
        if quote_amount_in == 0 || self.best_ask_price == 0 || self.best_ask_size == 0 {
            return 0;
        }

        // Apply taker fee
        let fee = quote_amount_in as u128 * self.taker_fee_bps as u128 / 10_000;
        let quote_after_fee = (quote_amount_in as u128).saturating_sub(fee);

        // Convert quote amount to base using best ask price
        // price is in quote_lots per base_unit, so base_out = quote_in / price
        let quote_in_lots = quote_after_fee / self.quote_lot_size as u128;
        if self.tick_size_in_quote_lots_per_base_unit == 0 {
            return 0;
        }

        let base_units_out = quote_in_lots * self.base_lot_size as u128
            / (self.best_ask_price as u128 * self.tick_size_in_quote_lots_per_base_unit as u128 / self.base_lot_size as u128).max(1);

        // Cap at available size
        let base_units_out = base_units_out.min(self.best_ask_size as u128 * self.base_lot_size as u128);

        base_units_out as u64
    }

    /// Sell base tokens for quote output, walking the bids.
    /// Returns quote token output amount after taker fee.
    pub fn calculate_sell_output(&self, base_amount_in: u64) -> u64 {
        if base_amount_in == 0 || self.best_bid_price == 0 || self.best_bid_size == 0 {
            return 0;
        }

        // Convert base to quote using best bid price
        let base_in_lots = base_amount_in as u128 / self.base_lot_size as u128;
        let quote_lots_out = base_in_lots * self.best_bid_price as u128
            * self.tick_size_in_quote_lots_per_base_unit as u128
            / self.base_lot_size as u128;

        let quote_out = quote_lots_out * self.quote_lot_size as u128;

        // Cap at available bid size
        let max_base_lots = self.best_bid_size as u128;
        let quote_out = if base_in_lots > max_base_lots {
            max_base_lots * self.best_bid_price as u128
                * self.tick_size_in_quote_lots_per_base_unit as u128
                * self.quote_lot_size as u128
                / self.base_lot_size as u128
        } else {
            quote_out
        };

        // Apply taker fee
        let fee = quote_out * self.taker_fee_bps as u128 / 10_000;
        let quote_after_fee = quote_out.saturating_sub(fee);

        quote_after_fee as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_market() -> PhoenixMarketState {
        PhoenixMarketState {
            discriminator: 0,
            base_mint: Pubkey::default(),
            quote_mint: Pubkey::default(),
            base_vault: Pubkey::default(),
            quote_vault: Pubkey::default(),
            base_lot_size: 1_000_000,       // 1e6 (6 decimals)
            quote_lot_size: 1_000,           // 1e3
            tick_size_in_quote_lots_per_base_unit: 1_000_000,
            order_sequence_number: 100,
            taker_fee_bps: 10,               // 0.1%
            best_bid_price: 150,             // 150 SOL per token
            best_bid_size: 1000,             // 1000 lots
            best_ask_price: 151,
            best_ask_size: 1000,
        }
    }

    #[test]
    fn test_phoenix_buy() {
        let market = test_market();
        let out = market.calculate_buy_output(1_000_000_000); // 1 SOL worth of quote
        assert!(out > 0);
    }

    #[test]
    fn test_phoenix_sell() {
        let market = test_market();
        let out = market.calculate_sell_output(1_000_000_000); // sell 1000 base tokens
        assert!(out > 0);
    }

    #[test]
    fn test_phoenix_zero_input() {
        let market = test_market();
        assert_eq!(market.calculate_buy_output(0), 0);
        assert_eq!(market.calculate_sell_output(0), 0);
    }

    #[test]
    fn test_phoenix_empty_book() {
        let mut market = test_market();
        market.best_ask_size = 0;
        market.best_bid_size = 0;
        assert_eq!(market.calculate_buy_output(1_000_000_000), 0);
        assert_eq!(market.calculate_sell_output(1_000_000_000), 0);
    }
}
