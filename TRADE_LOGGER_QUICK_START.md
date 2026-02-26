# Trade Logger - Quick Reference

## ✅ Installed & Working

The CSV trade logging system is now fully implemented and compiled successfully.

---

## Quick Start

### 1. Initialize Logger

```rust
use solana_mev_bot::chain::trade_logger::{TradeLogger, ExecutedTrade};

let logger = TradeLogger::new("trades.csv");
```

### 2. Log a Trade

```rust
let trade = ExecutedTrade {
    timestamp: chrono::Local::now().to_rfc3339(),
    token_mint: "EPjFWaJtkqQ6pe9EKUs3G2CavwEyKq6SL8xszduP98d".to_string(),
    buy_dex: "raydium".to_string(),
    sell_dex: "pump".to_string(),
    input_sol: 1.0,
    expected_profit_sol: 0.007,
    expected_profit_pct: 0.7,
    tx_signature: "sig123...".to_string(),
    status: "success".to_string(),
    actual_profit_sol: Some(0.0065),
    notes: "USDC arbitrage".to_string(),
};

logger.log_trade(&trade)?;
```

### 3. View Trades

```bash
# View all trades as CSV
cat trades.csv

# View last 10 trades
tail -10 trades.csv

# Count successful trades
grep "success" trades.csv | wc -l

# Total profit (using awk)
awk -F',' 'NR>1 {sum += $10} END {print "Total: " sum " SOL"}' trades.csv
```

---

## API Reference

### TradeLogger Methods

| Method | Purpose | Example |
|--------|---------|---------|
| `new(filename)` | Create logger | `TradeLogger::new("trades.csv")` |
| `log_trade(trade)` | Log a single trade | `logger.log_trade(&trade)?` |
| `log_message(msg)` | Print timestamped message | `logger.log_message("Trade executed")?` |
| `read_all_trades()` | Load all trades from file | `let trades = logger.read_all_trades()?` |
| `calculate_stats()` | Get trade statistics | `let stats = logger.calculate_stats()?` |
| `print_summary()` | Print formatted stats | `logger.print_summary()?` |
| `get_file_path()` | Get log file path | `let path = logger.get_file_path()` |

### ExecutedTrade Fields

```rust
pub struct ExecutedTrade {
    pub timestamp: String,              // ISO 8601 timestamp
    pub token_mint: String,             // Token address
    pub buy_dex: String,                // "raydium", "pump", "whirlpool", etc
    pub sell_dex: String,               // Same format
    pub input_sol: f64,                 // Capital deployed (SOL)
    pub expected_profit_sol: f64,       // Expected profit (SOL)
    pub expected_profit_pct: f64,       // Expected profit (%)
    pub tx_signature: String,           // Solana transaction hash
    pub status: String,                 // "pending", "success", "failed"
    pub actual_profit_sol: Option<f64>, // Actual profit after execution
    pub notes: String,                  // Additional notes
}
```

---

## Usage Examples

### Example 1: Log and Track

```rust
use solana_mev_bot::chain::trade_logger::{TradeLogger, ExecutedTrade};
use chrono::Local;

#[tokio::main]
async fn main() {
    let logger = TradeLogger::new("bot_trades.csv");

    // After finding an opportunity and building tx...
    let trade = ExecutedTrade {
        timestamp: Local::now().to_rfc3339(),
        token_mint: "...".to_string(),
        buy_dex: "raydium".to_string(),
        sell_dex: "pump".to_string(),
        input_sol: 1.0,
        expected_profit_sol: 0.01,
        expected_profit_pct: 1.0,
        tx_signature: tx_sig.to_string(),
        status: "success".to_string(),
        actual_profit_sol: Some(0.009),
        notes: "".to_string(),
    };

    logger.log_trade(&trade)?;
    logger.print_summary()?;
}
```

### Example 2: Analyze Trades

```rust
let logger = TradeLogger::new("trades.csv");

// Get statistics
let stats = logger.calculate_stats()?;
println!("Success rate: {:.1}%", stats.success_rate);
println!("Total profit: {:.4} SOL", stats.total_profit_sol);
println!("Avg per trade: {:.6} SOL", stats.average_profit_sol);

// Print formatted summary
logger.print_summary()?;

// Read all trades programmatically
let trades = logger.read_all_trades()?;
for trade in trades {
    println!("{} profit: {:.4} SOL", 
        trade.status, 
        trade.actual_profit_sol.unwrap_or(0.0)
    );
}
```

### Example 3: Query Specific Trades

```bash
# USDC trades only
grep "EPjFWaJtkqQ6pe9EKUs3G2CavwEyKq6SL8xszduP98d" trades.csv

# Raydium → Pump trades
awk -F',' '$3=="raydium" && $4=="pump"' trades.csv

# Profitable trades (actual > expected)
awk -F',' 'NR>1 && $10 > $6' trades.csv

# Failed trades with notes
grep "failed" trades.csv | awk -F',' '{print $1, $11}'
```

---

## CSV File Format

Example `trades.csv`:

```
timestamp,token_mint,buy_dex,sell_dex,input_sol,expected_profit_sol,expected_profit_pct,tx_signature,status,actual_profit_sol,notes
2026-01-09T23:45:12Z,EPjFWaJ...,raydium,pump,1.000000,0.007000,0.70,5gfnfnj...,success,0.006500,
2026-01-09T23:46:21Z,So11111...,whirlpool,raydium,0.500000,0.003000,0.60,5gfnfny...,failed,0.000000,Slippage exceeded
2026-01-09T23:47:33Z,EPjFWaJ...,pump,raydium,2.000000,0.015000,0.75,pending...,pending,0.000000,Awaiting confirmation
```

---

## Integration with Bot

### In Your Main Loop

```rust
#[tokio::main]
async fn main() {
    let logger = TradeLogger::new("trades.csv");
    
    loop {
        // Find opportunities
        let opps = detector.find_opportunities(&pool_data);
        
        if let Some(opp) = opps.first() {
            // Log before execution
            let mut trade = ExecutedTrade {
                timestamp: Local::now().to_rfc3339(),
                token_mint: opp.token_mint.clone(),
                buy_dex: opp.path[0].dex.clone(),
                sell_dex: opp.path[1].dex.clone(),
                input_sol: opp.input_amount_sol,
                expected_profit_sol: opp.gross_profit_sol,
                expected_profit_pct: opp.profit_percent,
                tx_signature: "pending".to_string(),
                status: "pending".to_string(),
                actual_profit_sol: None,
                notes: format!("Risk: {}, Confidence: {}", 
                    opp.risk_score, opp.confidence_score),
            };
            
            logger.log_trade(&trade).ok();
            
            // Execute trade
            match execute_arbitrage(&opp).await {
                Ok(sig) => {
                    trade.tx_signature = sig.to_string();
                    trade.status = "success".to_string();
                    trade.actual_profit_sol = Some(opp.gross_profit_sol * 0.95); // After slippage
                    logger.log_trade(&trade).ok();
                }
                Err(e) => {
                    trade.tx_signature = "failed".to_string();
                    trade.status = "failed".to_string();
                    trade.notes = format!("Error: {}", e);
                    logger.log_trade(&trade).ok();
                }
            }
        }
        
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
```

---

## Analyzing Results

### Python Analysis Script

```python
import csv
from collections import defaultdict

def analyze_trades(filename="trades.csv"):
    trades = []
    with open(filename) as f:
        reader = csv.DictReader(f)
        trades = list(reader)
    
    # Summary stats
    successful = [t for t in trades if t['status'] == 'success']
    total_profit = sum(float(t['actual_profit_sol']) for t in successful)
    
    print(f"Total Trades: {len(trades)}")
    print(f"Successful: {len(successful)} ({100*len(successful)/len(trades):.1f}%)")
    print(f"Total Profit: {total_profit:.4f} SOL")
    print(f"Avg Profit: {total_profit/len(successful):.6f} SOL")
    
    # By DEX pair
    by_pair = defaultdict(lambda: {'count': 0, 'profit': 0})
    for t in successful:
        pair = f"{t['buy_dex']}->{t['sell_dex']}"
        by_pair[pair]['count'] += 1
        by_pair[pair]['profit'] += float(t['actual_profit_sol'])
    
    print("\nProfits by DEX Pair:")
    for pair, stats in sorted(by_pair.items(), 
                             key=lambda x: x[1]['profit'], 
                             reverse=True):
        print(f"  {pair}: {stats['count']} trades, {stats['profit']:.4f} SOL")

analyze_trades()
```

Run it:
```bash
python3 analyze_trades.py
```

---

## Dashboard Monitoring

### Bash One-Liner for Live Stats

```bash
watch -n 5 'echo "=== TRADE SUMMARY ===" && echo "Total: $(tail -n +2 trades.csv | wc -l)" && echo "Profit: $(awk -F"," '"'"'NR>1 {sum+=$10} END {print sum}'"'"' trades.csv) SOL"'
```

### View Last 10 Trades Formatted

```bash
tail -10 trades.csv | awk -F',' '{printf "%-20s %s->%s  %.4f SOL (%.2f%%)\n", 
    substr($1,1,19), $3, $4, $10, $7}'
```

---

## Features

✅ **Automatic CSV creation** with headers
✅ **Handles commas in notes** (converts to semicolons)
✅ **Timestamps** for every trade
✅ **Track expected vs actual profit**
✅ **Status tracking** (pending, success, failed)
✅ **Statistics calculation** (success rate, total profit, averages)
✅ **Pretty-printed summary output**
✅ **Read existing trades** from CSV
✅ **No dependencies** beyond chrono (already included)
✅ **Thread-safe** with async support

---

## Testing

Run the included test:

```bash
cargo test trade_logger --lib
```

Or run the example:

```bash
cargo run --example trade_logger_example
```

---

## Next Steps

1. **Integrate into main bot** - Call `logger.log_trade()` when executing
2. **Set up monitoring** - Use `watch` or Python script to monitor performance
3. **Analyze results** - Run Python script weekly to analyze trends
4. **Optimize based on data** - Use logs to identify best DEX pairs

---

## File Location

- **Module**: `src/chain/trade_logger.rs`
- **Example**: `examples/trade_logger_example.rs`
- **Log file**: `trades.csv` (created automatically)

---

## Status

✅ **Build**: Successful
✅ **Module**: Compiled
✅ **Tests**: Included
✅ **Example**: Ready to run
✅ **Documentation**: Complete

You can now track every trade your bot executes!
