# Trade Tracking Guide

## Current Status

**❌ Trade tracking is NOT currently implemented in your bot.**

The bot can detect opportunities and build transactions, but it doesn't:
- Log executed trades
- Track profit/loss
- Record transaction signatures
- Store trading history

This guide shows you 3 ways to add trade tracking.

---

## Option 1: Simple File-Based Logging (Easiest)

Perfect for small-scale testing. Appends trades to a CSV file.

### Setup

Create a new file: `src/chain/trade_logger.rs`

```rust
use std::fs::OpenOptions;
use std::io::Write;
use serde::{Deserialize, Serialize};
use chrono::Local;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutedTrade {
    pub timestamp: String,           // When executed
    pub token_mint: String,          // Token traded
    pub buy_dex: String,             // Where we bought
    pub sell_dex: String,            // Where we sold
    pub input_sol: f64,              // Capital deployed
    pub expected_profit_sol: f64,     // Expected profit
    pub expected_profit_pct: f64,     // Expected profit %
    pub tx_signature: String,        // Solana transaction sig
    pub status: String,              // "pending", "success", "failed"
    pub actual_profit_sol: Option<f64>, // Actual profit (after execution)
    pub notes: String,               // Any notes
}

pub struct TradeLogger {
    log_file: String,
}

impl TradeLogger {
    pub fn new(filename: &str) -> Self {
        // Create file with headers if it doesn't exist
        if !std::path::Path::new(filename).exists() {
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .open(filename)
                .expect("Failed to create log file");
            
            let header = "timestamp,token_mint,buy_dex,sell_dex,input_sol,expected_profit_sol,expected_profit_pct,tx_signature,status,actual_profit_sol,notes\n";
            file.write_all(header.as_bytes()).expect("Failed to write header");
        }
        
        TradeLogger {
            log_file: filename.to_string(),
        }
    }
    
    pub fn log_trade(&self, trade: &ExecutedTrade) -> Result<(), Box<dyn std::error::Error>> {
        let mut file = OpenOptions::new()
            .append(true)
            .open(&self.log_file)?;
        
        let csv_line = format!(
            "{},{},{},{},{:.4},{:.4},{:.2},{},{},{},{}\n",
            trade.timestamp,
            trade.token_mint,
            trade.buy_dex,
            trade.sell_dex,
            trade.input_sol,
            trade.expected_profit_sol,
            trade.expected_profit_pct,
            trade.tx_signature,
            trade.status,
            trade.actual_profit_sol.unwrap_or(0.0),
            trade.notes
        );
        
        file.write_all(csv_line.as_bytes())?;
        Ok(())
    }
    
    pub fn log_simple(&self, message: &str) -> Result<(), Box<dyn std::error::Error>> {
        let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        println!("[{}] {}", timestamp, message);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_log_trade() {
        let logger = TradeLogger::new("test_trades.csv");
        
        let trade = ExecutedTrade {
            timestamp: Local::now().to_rfc3339(),
            token_mint: "EPjFWaJtkqQ6pe9EKUs3G2CavwEyKq6SL8xszduP98d".to_string(),
            buy_dex: "raydium".to_string(),
            sell_dex: "pump".to_string(),
            input_sol: 1.0,
            expected_profit_sol: 0.01,
            expected_profit_pct: 1.0,
            tx_signature: "5gfnfnjkfn...".to_string(),
            status: "success".to_string(),
            actual_profit_sol: Some(0.009),
            notes: "Test trade".to_string(),
        };
        
        assert!(logger.log_trade(&trade).is_ok());
    }
}
```

### Register in mod.rs

Add to `src/chain/mod.rs`:
```rust
pub mod trade_logger;
```

### Use in Your Bot

```rust
use solana_mev_bot::chain::trade_logger::{TradeLogger, ExecutedTrade};
use chrono::Local;

#[tokio::main]
async fn main() {
    let logger = TradeLogger::new("trades.csv");
    
    // When executing a trade:
    let trade = ExecutedTrade {
        timestamp: Local::now().to_rfc3339(),
        token_mint: "EPjFWaJtkqQ6pe9EKUs3G2CavwEyKq6SL8xszduP98d".to_string(),
        buy_dex: "raydium".to_string(),
        sell_dex: "pump".to_string(),
        input_sol: 1.0,
        expected_profit_sol: 0.007,
        expected_profit_pct: 0.7,
        tx_signature: sig.to_string(),
        status: "success".to_string(),
        actual_profit_sol: Some(0.0065),
        notes: "USDC arbitrage".to_string(),
    };
    
    logger.log_trade(&trade).expect("Failed to log trade");
}
```

### View Your Trades

```bash
# View all trades
cat trades.csv

# View last 10 trades
tail -10 trades.csv

# View trades with "success" status
grep "success" trades.csv

# Calculate total profit
awk -F',' '{sum += $6} END {print "Total profit: " sum " SOL"}' trades.csv
```

---

## Option 2: JSON-Based Database (Better)

More structured, easier to query. Perfect for medium-scale operations.

### Create `src/chain/trade_db.rs`

```rust
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeRecord {
    pub id: u64,
    pub timestamp: String,
    pub token_mint: String,
    pub buy_dex: String,
    pub sell_dex: String,
    pub input_sol: f64,
    pub expected_profit: TradeProfit,
    pub actual_profit: Option<TradeProfit>,
    pub tx_signature: String,
    pub status: TradeStatus,
    pub fees_sol: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeProfit {
    pub sol: f64,
    pub percent: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TradeStatus {
    Pending,
    Success,
    Failed,
    Reverted,
}

pub struct TradeDatabase {
    db_file: String,
    trades: Vec<TradeRecord>,
}

impl TradeDatabase {
    pub fn new(db_file: &str) -> Self {
        let mut trades = Vec::new();
        
        if Path::new(db_file).exists() {
            if let Ok(content) = fs::read_to_string(db_file) {
                if let Ok(loaded) = serde_json::from_str::<Vec<TradeRecord>>(&content) {
                    trades = loaded;
                }
            }
        }
        
        TradeDatabase {
            db_file: db_file.to_string(),
            trades,
        }
    }
    
    pub fn add_trade(&mut self, mut trade: TradeRecord) -> u64 {
        trade.id = self.trades.len() as u64 + 1;
        self.trades.push(trade);
        self.save().ok();
        trade.id
    }
    
    pub fn update_trade(&mut self, id: u64, trade: TradeRecord) {
        if let Some(pos) = self.trades.iter().position(|t| t.id == id) {
            self.trades[pos] = trade;
            self.save().ok();
        }
    }
    
    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let json = serde_json::to_string_pretty(&self.trades)?;
        fs::write(&self.db_file, json)?;
        Ok(())
    }
    
    pub fn get_all(&self) -> Vec<TradeRecord> {
        self.trades.clone()
    }
    
    pub fn get_by_status(&self, status: &TradeStatus) -> Vec<TradeRecord> {
        self.trades.iter()
            .filter(|t| std::mem::discriminant(&t.status) == std::mem::discriminant(status))
            .cloned()
            .collect()
    }
    
    pub fn get_stats(&self) -> TradeStats {
        let successful = self.trades.iter()
            .filter(|t| matches!(t.status, TradeStatus::Success))
            .collect::<Vec<_>>();
        
        let total_profit: f64 = successful.iter()
            .filter_map(|t| t.actual_profit.as_ref())
            .map(|p| p.sol)
            .sum();
        
        let total_trades = self.trades.len();
        let success_count = successful.len();
        let avg_profit = if success_count > 0 {
            total_profit / success_count as f64
        } else {
            0.0
        };
        
        TradeStats {
            total_trades,
            successful_trades: success_count,
            failed_trades: total_trades - success_count,
            total_profit_sol: total_profit,
            average_profit_sol: avg_profit,
            success_rate: (success_count as f64 / total_trades as f64) * 100.0,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TradeStats {
    pub total_trades: usize,
    pub successful_trades: usize,
    pub failed_trades: usize,
    pub total_profit_sol: f64,
    pub average_profit_sol: f64,
    pub success_rate: f64,
}
```

### Use in Bot

```rust
use solana_mev_bot::chain::trade_db::{TradeDatabase, TradeRecord, TradeProfit, TradeStatus};

#[tokio::main]
async fn main() {
    let mut db = TradeDatabase::new("trades.json");
    
    // Log a new trade
    let trade = TradeRecord {
        id: 0, // Will be assigned
        timestamp: chrono::Local::now().to_rfc3339(),
        token_mint: "EPjFWaJtkqQ6pe9EKUs3G2CavwEyKq6SL8xszduP98d".to_string(),
        buy_dex: "raydium".to_string(),
        sell_dex: "pump".to_string(),
        input_sol: 1.0,
        expected_profit: TradeProfit { sol: 0.007, percent: 0.7 },
        actual_profit: None,
        tx_signature: sig.to_string(),
        status: TradeStatus::Pending,
        fees_sol: 0.00025,
    };
    
    let trade_id = db.add_trade(trade);
    println!("Trade #{} recorded", trade_id);
    
    // Later, update with actual results
    if let Some(mut t) = db.get_all().into_iter().find(|t| t.id == trade_id) {
        t.status = TradeStatus::Success;
        t.actual_profit = Some(TradeProfit { sol: 0.0065, percent: 0.65 });
        db.update_trade(trade_id, t);
    }
    
    // View stats
    let stats = db.get_stats();
    println!("Total profit: {:.4} SOL", stats.total_profit_sol);
    println!("Success rate: {:.1}%", stats.success_rate);
    println!("Avg profit per trade: {:.6} SOL", stats.average_profit_sol);
}
```

### Query Your Trades

```bash
# View as JSON
cat trades.json | jq '.' 

# Get success count
cat trades.json | jq '[.[] | select(.status == "success")] | length'

# Total profit
cat trades.json | jq '[.[] | select(.actual_profit != null) | .actual_profit.sol] | add'

# Filter by token
cat trades.json | jq '.[] | select(.token_mint == "EPjFWaJ...")'
```

---

## Option 3: Remote Logging (Production)

Send trades to a remote database or API. Best for production bots.

### Example: Send to Webhook

```rust
use reqwest::Client;

pub async fn log_trade_remote(trade: &TradeRecord) -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new();
    
    // Send to your tracking server
    client
        .post("https://your-server.com/api/trades")
        .json(trade)
        .send()
        .await?;
    
    Ok(())
}

// Or send to Discord
pub async fn notify_discord(trade: &TradeRecord) -> Result<(), Box<dyn std::error::Error>> {
    let webhook_url = std::env::var("DISCORD_WEBHOOK")?;
    
    let client = Client::new();
    
    let message = format!(
        "🎯 Trade Executed\n{} → {}\n💰 {:.4} SOL profit ({:.2}%)",
        trade.buy_dex, trade.sell_dex, 
        trade.actual_profit.as_ref().map(|p| p.sol).unwrap_or(0.0),
        trade.expected_profit.percent
    );
    
    client
        .post(&webhook_url)
        .json(&serde_json::json!({
            "content": message
        }))
        .send()
        .await?;
    
    Ok(())
}
```

---

## Monitoring Your Trades

### Real-Time Dashboard

Create `scripts/monitor_trades.sh`:

```bash
#!/bin/bash

while true; do
    clear
    echo "========== TRADE MONITORING =========="
    echo "Last updated: $(date)"
    echo ""
    
    if [ -f "trades.csv" ]; then
        echo "📊 STATISTICS:"
        echo "Total trades: $(tail -n +2 trades.csv | wc -l)"
        echo "Success rate: $(grep -c "success" trades.csv) success"
        echo "Failed: $(grep -c "failed" trades.csv) failed"
        echo ""
        
        echo "💰 RECENT TRADES:"
        tail -5 trades.csv | awk -F',' '{
            printf "%-20s %s→%s  %.4f SOL (%.2f%%)\n",
            $1, $3, $4, $6, $7
        }'
        echo ""
    fi
    
    sleep 10
done
```

Run it:
```bash
chmod +x scripts/monitor_trades.sh
./scripts/monitor_trades.sh
```

### Python Analysis Script

Create `scripts/analyze_trades.py`:

```python
import csv
import json
from datetime import datetime

def analyze_csv(filename):
    trades = []
    with open(filename) as f:
        reader = csv.DictReader(f)
        trades = list(reader)
    
    successful = [t for t in trades if t['status'] == 'success']
    
    print(f"Total Trades: {len(trades)}")
    print(f"Successful: {len(successful)}")
    print(f"Success Rate: {100 * len(successful) / len(trades):.1f}%")
    
    total_profit = sum(float(t['expected_profit_sol']) for t in successful)
    print(f"Total Profit: {total_profit:.4f} SOL")
    
    # By DEX pair
    by_pair = {}
    for t in successful:
        pair = f"{t['buy_dex']}->{t['sell_dex']}"
        if pair not in by_pair:
            by_pair[pair] = {'count': 0, 'profit': 0}
        by_pair[pair]['count'] += 1
        by_pair[pair]['profit'] += float(t['expected_profit_sol'])
    
    print("\nProfits by DEX Pair:")
    for pair, stats in sorted(by_pair.items(), key=lambda x: x[1]['profit'], reverse=True):
        print(f"  {pair}: {stats['count']} trades, {stats['profit']:.4f} SOL")

if __name__ == "__main__":
    analyze_csv("trades.csv")
```

Run it:
```bash
python scripts/analyze_trades.py
```

---

## Recommended Setup

For your current bot, I recommend **Option 1 (CSV Logging)** because:

✅ **Easiest to implement** - Just 50 lines of code
✅ **Human-readable** - Open in Excel/Google Sheets
✅ **Easy to analyze** - Use `grep`, `awk`, or Python
✅ **Minimal dependencies** - No database required
✅ **Good for debugging** - See exactly what happened

### Quick Implementation

1. **Add dependency to Cargo.toml**:
   ```toml
   chrono = "0.4"
   ```

2. **Create `src/chain/trade_logger.rs`** (code above)

3. **Add to `src/chain/mod.rs`**:
   ```rust
   pub mod trade_logger;
   ```

4. **Use in main loop**:
   ```rust
   let logger = TradeLogger::new("trades.csv");
   logger.log_trade(&ExecutedTrade { ... }).ok();
   ```

5. **View results**:
   ```bash
   tail -20 trades.csv
   ```

---

## What Gets Tracked

Each trade log entry captures:

| Field | Purpose |
|-------|---------|
| **timestamp** | When the trade was executed |
| **token_mint** | Which token was traded |
| **buy_dex** | Where we bought (Raydium, Pump, etc) |
| **sell_dex** | Where we sold |
| **input_sol** | Capital deployed |
| **expected_profit_sol** | Expected profit in SOL |
| **expected_profit_pct** | Expected profit as % |
| **tx_signature** | Solana transaction ID (for verification) |
| **status** | success/failed/pending |
| **actual_profit_sol** | Real profit after execution |
| **notes** | Any issues or observations |

---

## Next Steps

1. Choose logging method (Option 1 recommended)
2. Implement trade logger module
3. Integrate into bot's execute_trade() function
4. Log every trade attempt
5. Analyze results monthly

This way you'll have complete visibility into bot performance and can optimize strategies based on real data.

Would you like me to implement one of these options for you?
