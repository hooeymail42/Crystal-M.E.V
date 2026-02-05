# Solana MEV Arbitrage Bot - Instruction Manual

Complete guide to setting up, configuring, and running the bot on Solana mainnet.

---

## Table of Contents

1. [What This Bot Does](#what-this-bot-does)
2. [Prerequisites](#prerequisites)
3. [Installation](#installation)
4. [Create a New Wallet](#create-a-new-wallet)
5. [Get an RPC Endpoint](#get-an-rpc-endpoint)
6. [Configure the Bot](#configure-the-bot)
7. [Test in Demo Mode](#test-in-demo-mode)
8. [Go Live on Mainnet](#go-live-on-mainnet)
9. [Finding Pool Addresses](#finding-pool-addresses)
10. [Enable Jito Bundles](#enable-jito-bundles)
11. [Enable Flash Loans](#enable-flash-loans)
12. [Monitoring and Logs](#monitoring-and-logs)
13. [Important Commands Reference](#important-commands-reference)
14. [Troubleshooting](#troubleshooting)
15. [Risk Warnings](#risk-warnings)

---

## What This Bot Does

This bot finds price differences for the same token across multiple decentralized exchanges (DEXes) on Solana. When Token X costs 1.00 SOL on Raydium but 1.03 SOL on Orca Whirlpool, the bot buys on Raydium and sells on Whirlpool in a single atomic transaction, keeping the 0.03 SOL difference as profit.

**Supported DEXes:** Raydium (V4, CP, CLMM), Orca Whirlpool, Meteora (DLMM, DAMM V2), Pump.fun, SolFi, Vertigo, Phoenix, Lifinity, Heaven.

**Key features:**
- Cross-DEX arbitrage (2-hop and 3-hop paths)
- Kamino Finance flash loans (trade with borrowed capital)
- Jito MEV bundles (priority transaction ordering)
- Auto-compounding (profits automatically become trading capital)
- Dynamic position sizing based on wallet balance and confidence
- Drawdown protection (auto-pauses if losing too much)

---

## Prerequisites

You need these installed on your computer before starting.

### 1. Rust Programming Language

```bash
# Install Rust (follow the prompts, choose default installation)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# After installation, reload your shell
source $HOME/.cargo/env

# Verify installation (need version 1.93.0 or higher)
rustc --version
```

### 2. Solana CLI Tools

```bash
# Install Solana CLI
sh -c "$(curl -sSfL https://release.anza.xyz/stable/install)"

# Add to your PATH (add this line to ~/.bashrc or ~/.zshrc)
export PATH="$HOME/.local/share/solana/install/active_release/bin:$PATH"

# Verify installation
solana --version
```

### 3. Build Tools (Linux/WSL)

```bash
# Ubuntu/Debian
sudo apt update
sudo apt install -y build-essential pkg-config libssl-dev

# macOS (install Xcode command line tools)
xcode-select --install
```

---

## Installation

### Clone and Build the Bot

```bash
# Navigate to where you want the bot
cd ~

# If you already have the code, skip cloning. Otherwise:
# git clone <your-repo-url> solana-mev-bot

# Enter the project directory
cd solana-mev-bot

# Build the bot (this takes a few minutes the first time)
cargo build --release

# Verify the build succeeded
ls -la target/release/solana-mev-bot
```

The compiled binary is at `target/release/solana-mev-bot`.

---

## Create a New Wallet

**CRITICAL: This wallet will hold real SOL. Treat the private key like a bank password. Never share it. Never paste it in a chat. Never commit it to git.**

### Option A: Using Solana CLI (Recommended)

```bash
# Generate a new keypair file
solana-keygen new --outfile ~/mev-wallet.json

# It will show you:
#   - Your PUBLIC address (safe to share, starts with a letter/number)
#   - A seed phrase (WRITE THIS DOWN ON PAPER, store it safely)
#
# Example output:
#   pubkey: 7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU
#   Save this seed phrase to recover your keypair:
#   [12 words here - WRITE THESE DOWN]

# View your public address anytime
solana-keygen pubkey ~/mev-wallet.json

# Export the private key as base58 (needed for the .env file)
# The entire contents of the JSON file is your private key in byte array format.
# To get base58 format, use:
cat ~/mev-wallet.json
# This outputs a JSON array of numbers like [123,45,67,...].
# You can convert this to base58 using:
python3 -c "
import json, base64
with open('$HOME/mev-wallet.json') as f:
    key_bytes = bytes(json.load(f))
# base58 encode
import hashlib
ALPHABET = b'123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'
n = int.from_bytes(key_bytes, 'big')
result = b''
while n > 0:
    n, r = divmod(n, 58)
    result = ALPHABET[r:r+1] + result
print(result.decode())
"
```

### Option B: Using solana-keygen for Base58 Directly

```bash
# Generate and immediately get the base58 keypair
solana-keygen new --no-bip39-passphrase --outfile ~/mev-wallet.json

# Get the base58 representation
solana-keygen pubkey ~/mev-wallet.json    # your public address
```

### Fund the Wallet

```bash
# Set your wallet as the default
solana config set --keypair ~/mev-wallet.json
solana config set --url https://api.mainnet-beta.solana.com

# Check balance (will be 0 initially)
solana balance

# Send SOL to your wallet address from an exchange (Coinbase, Binance, etc.)
# or from another wallet. You need the PUBLIC address shown above.
#
# Recommended starting amount: 0.5 - 2 SOL
#   - 0.05 SOL is kept as reserve for transaction fees
#   - The rest is used for trading
#   - More capital = larger position sizes = more potential profit
```

---

## Get an RPC Endpoint

The free public Solana RPC (`https://api.mainnet-beta.solana.com`) is rate-limited and slow. For a trading bot, you need a paid RPC.

### Recommended Providers (in order)

| Provider | Free Tier | Website |
|----------|-----------|---------|
| **Helius** | 50K requests/day | https://helius.dev |
| **QuickNode** | Limited free | https://quicknode.com |
| **Triton** | No free tier | https://triton.one |
| **Alchemy** | 300M CU/month | https://alchemy.com |

### How to Get a Helius Key (Example)

1. Go to https://helius.dev
2. Sign up for a free account
3. Create a new project
4. Copy your API key
5. Your RPC URL will be: `https://mainnet.helius-rpc.com/?api-key=YOUR_KEY_HERE`
6. Your WebSocket URL will be: `wss://mainnet.helius-rpc.com/?api-key=YOUR_KEY_HERE`

---

## Configure the Bot

All configuration is done through a `.env` file in the project root.

### Create Your .env File

```bash
# From the project directory
cd ~/solana-mev-bot

# Create a fresh .env file
cat > .env << 'ENDOFFILE'
# =============================================================
# SOLANA MEV BOT CONFIGURATION
# =============================================================

# --- RPC Connection ---
# Replace with your paid RPC URL (see "Get an RPC Endpoint" section)
RPC_URL=https://mainnet.helius-rpc.com/?api-key=YOUR_API_KEY_HERE
WS_URL=wss://mainnet.helius-rpc.com/?api-key=YOUR_API_KEY_HERE

# --- Wallet ---
# Your wallet's private key in base58 format
# WARNING: Never share this. Never commit this file to git.
SOLANA_KEYPAIR=YOUR_BASE58_PRIVATE_KEY_HERE

# --- Safety Settings ---
# IMPORTANT: Start with false! Only set to true when you're ready.
ENABLE_REAL_EXECUTION=false

# --- Trading Parameters ---
# Minimum profit in SOL to execute a trade (0.001 = ~$0.20 at $200/SOL)
MIN_PROFIT_SOL=0.001

# Maximum acceptable slippage as percentage (1.0 = 1%)
MAX_SLIPPAGE_PCT=1.0

# Transaction compute unit budget (higher = more complex trades allowed)
BOT_COMPUTE_UNIT_LIMIT=600000

# Priority fee in lamports (higher = faster inclusion, but costs more)
# 10000 lamports = 0.00001 SOL per transaction
PRIORITY_FEE_LAMPORTS=10000

# How fast the bot loops (milliseconds)
# 500 = check for opportunities twice per second
LOOP_INTERVAL_MS=500

# How often to refresh pool data from chain (milliseconds)
REFRESH_INTERVAL_MS=2000

# --- Jito MEV Bundles (Optional - recommended for production) ---
JITO_ENABLED=false
JITO_TIP_LAMPORTS=10000
JITO_BLOCK_ENGINE_URL=https://mainnet.block-engine.jito.wtf

# --- Flash Loans (Optional - for trading with borrowed capital) ---
FLASHLOAN_ENABLED=false
FLASHLOAN_RESERVE=
FLASHLOAN_RESERVE_VAULT=
FLASHLOAN_FEE_RECEIVER=

# --- Multi-RPC Spam (Optional) ---
SPAM_ENABLED=false
SPAM_SENDING_RPC_URLS=

# --- Token Mints and Pool Addresses ---
# Each MINT_N is a token you want to arbitrage.
# For each mint, list the pool addresses on each DEX (comma-separated).
# You need at least 2 pools across different DEXes for arbitrage to work.
#
# Example: USDC across Raydium and Whirlpool
MINT_1=EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v
MINT_1_RAYDIUM_POOL_LIST=58oQChx4yWmvKdwLLZzBi4ChoCc2fqCUWBkwMihLYQo2
MINT_1_RAYDIUM_CP_POOL_LIST=
MINT_1_RAYDIUM_CLMM_POOL_LIST=
MINT_1_WHIRLPOOL_POOL_LIST=7qbRF6YsyGuLUVs6Y1q64bdVrfe4ZcUUz1JRdoVNUJnm
MINT_1_DLMM_POOL_LIST=
MINT_1_METEORA_DAMM_POOL_LIST=
MINT_1_METEORA_DAMM_V2_POOL_LIST=
MINT_1_PUMP_POOL_LIST=
MINT_1_SOLFI_POOL_LIST=
MINT_1_VERTIGO_POOL_LIST=
MINT_1_PHOENIX_POOL_LIST=
MINT_1_LIFINITY_POOL_LIST=
MINT_1_HEAVEN_POOL_LIST=

# You can add up to 10 mints (MINT_2, MINT_3, ... MINT_10)
# MINT_2=<another_token_mint_address>
# MINT_2_RAYDIUM_POOL_LIST=<pool_address>
# ... etc
ENDOFFILE
```

### Edit the .env File

```bash
# Open in your preferred editor
nano .env
# or
vim .env
```

**You MUST change these three values:**
1. `RPC_URL` and `WS_URL` — your paid RPC endpoint
2. `SOLANA_KEYPAIR` — your wallet's base58 private key
3. `MINT_*` pool addresses — the pools you want to arbitrage

---

## Test in Demo Mode

**Always test in demo mode first.** When `ENABLE_REAL_EXECUTION=false`, the bot simulates everything but never sends real transactions.

```bash
# Build and run in demo mode
cd ~/solana-mev-bot
cargo run --release
```

### What to Look For

```
# Good - bot starts successfully:
INFO  Solana MEV Arbitrage Bot starting...
INFO  Config loaded: RPC=https://..., mints=1, real_execution=false
INFO  Wallet: 7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU
INFO  Wallet balance: 1.5000 SOL
INFO  CapitalManager initialized: balance=1.5000 SOL, reserve=0.0500 SOL, ...
INFO  [DEMO MODE] Set ENABLE_REAL_EXECUTION=true to go live

# Good - finding opportunities:
INFO  [Loop 42] Found 2 opportunities for mint EPjFWdd5...
INFO    #1: Raydium -> Whirlpool | profit=0.002341 SOL (0.47%) | confidence=75

# Good - simulating execution:
INFO  [DEMO] Would send transaction with 4 instructions

# Periodic status (every 100 loops):
INFO  [Status] Loop=100, Opportunities=15, Executions=3, Mints=1
INFO  [Capital] balance=1.5000 SOL | available=1.4500 SOL | profit=0.0000 SOL ...
```

### If You See Errors

```
# "Could not fetch balance" - bad RPC URL or rate limited
# Fix: check your RPC_URL in .env

# "No pools to subscribe" - no pool addresses configured
# Fix: add pool addresses to your .env (see Finding Pool Addresses section)

# No opportunities found - normal if pools are well-arbitraged
# This doesn't mean the bot is broken. Wait and watch.
```

**Run in demo mode for at least 30 minutes** to verify the bot is finding opportunities before going live.

---

## Go Live on Mainnet

**Only do this after successful demo testing. You are risking real money.**

### Step 1: Ensure Wallet is Funded

```bash
solana config set --keypair ~/mev-wallet.json
solana balance
# Must show at least 0.1 SOL (0.5+ SOL recommended)
```

### Step 2: Enable Real Execution

Edit your `.env` file:

```bash
nano .env
```

Change this one line:

```
ENABLE_REAL_EXECUTION=true
```

### Step 3: Start the Bot

```bash
# Run in foreground (see output directly)
cargo run --release

# OR run in background with logging to a file
cargo run --release > bot_output.log 2>&1 &

# OR use screen/tmux for persistent sessions
screen -S mevbot
cargo run --release
# Press Ctrl+A then D to detach
# Reattach later with: screen -r mevbot
```

### Step 4: Monitor

```bash
# If running in background, watch the log
tail -f bot_output.log

# Check wallet balance
solana balance
```

### Stopping the Bot

```bash
# If running in foreground: press Ctrl+C

# If running in background:
# Find the process
ps aux | grep solana-mev-bot
# Kill it
kill <PID>

# If using screen:
screen -r mevbot
# Then Ctrl+C
```

---

## Finding Pool Addresses

You need the on-chain pool addresses for each DEX. Here's how to find them.

### Method 1: Solscan / Solana Explorer

1. Go to https://solscan.io
2. Search for the token mint address (e.g., USDC: `EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v`)
3. Click the "Markets" tab
4. You'll see pools listed by DEX with their addresses

### Method 2: DEX Websites

| DEX | Where to Find Pools |
|-----|---------------------|
| Raydium | https://raydium.io/liquidity/ - search token, pool address is in the URL |
| Orca Whirlpool | https://www.orca.so/ - check pool details |
| Meteora | https://app.meteora.ag/pools - search for token pair |
| Jupiter | https://jup.ag - shows available routes/pools |

### Method 3: On-Chain Programs

Each DEX has a program ID. You can query accounts owned by these programs:

```
Raydium V4 AMM:      675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8
Raydium CLMM:        CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK
Orca Whirlpool:       whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc
Meteora DLMM:        LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo
Meteora DAMM V2:     cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG
Pump.fun:            6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P
Phoenix:             PhoeNiXZ8ByJGLkxNfZRnkUfjvmuYqLR89jjFHGqdXY
Lifinity V2:         EewxydAPCCVuNEyrVN68PuSYdQ7wKn27V9Gjeoi8dy3S
SolFi:               SoLFiHG9TfgtdUXUjWAxi3LtvYuFyDLVhBWxdMZxyCe
```

### Example: Adding SOL/USDC Pools

```env
# USDC token mint
MINT_1=EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v

# Raydium SOL/USDC pool
MINT_1_RAYDIUM_POOL_LIST=58oQChx4yWmvKdwLLZzBi4ChoCc2fqCUWBkwMihLYQo2

# Orca Whirlpool SOL/USDC pool
MINT_1_WHIRLPOOL_POOL_LIST=7qbRF6YsyGuLUVs6Y1q64bdVrfe4ZcUUz1JRdoVNUJnm

# Multiple pools on same DEX (comma-separated, no spaces)
MINT_1_RAYDIUM_POOL_LIST=pool1address,pool2address,pool3address
```

**Tip:** The more pools you add across different DEXes for the same token, the more arbitrage paths the bot can find.

---

## Enable Jito Bundles

Jito bundles give your transaction priority ordering. Strongly recommended for production.

```env
JITO_ENABLED=true

# Tip amount in lamports (10000 = 0.00001 SOL per transaction)
# Higher tip = more likely to be included, but costs more
# Start with 10000, increase if transactions are being dropped
JITO_TIP_LAMPORTS=10000

# Jito block engine URL (default is correct for mainnet)
JITO_BLOCK_ENGINE_URL=https://mainnet.block-engine.jito.wtf
```

---

## Enable Flash Loans

Flash loans let you borrow SOL for a single transaction (borrow -> arbitrage -> repay in one atomic TX). This means you can execute larger trades than your wallet balance allows. The fee is ~0.09%.

The bot uses Kamino Finance (KLend) flash loans. You need the reserve account addresses for the SOL lending pool.

```env
FLASHLOAN_ENABLED=true

# Kamino SOL reserve accounts (these are the mainnet addresses)
# You need to look these up from Kamino's documentation or on-chain
FLASHLOAN_RESERVE=<kamino_sol_reserve_address>
FLASHLOAN_RESERVE_VAULT=<kamino_sol_reserve_vault_address>
FLASHLOAN_FEE_RECEIVER=<kamino_fee_receiver_address>
```

The bot automatically optimizes flash loan borrow amounts:
- If you have enough capital: only borrows if 3x leverage nets >10% more profit
- If you lack capital: borrows the deficit, only if profit exceeds the 0.09% fee

---

## Monitoring and Logs

### Log Output Explained

```
# Opportunity found:
[Loop 42] Found 2 opportunities for mint EPjFWdd5...
  #1: Raydium -> Whirlpool | profit=0.002341 SOL (0.47%) | confidence=75
       ^^^^^^^^    ^^^^^^^^^          ^^^^^^^^      ^^^^          ^^
       Buy DEX     Sell DEX     Expected profit   Profit%    0-100 score

# Capital status (every 100 loops):
[Capital] balance=1.5234 SOL | available=1.4734 SOL | profit=0.0234 SOL |
trades=15 (W:12 L:3) | win_rate=80.0% | drawdown=0.0% | risk=10.0%
  ^^^^^^^               ^^^^^^^^^          ^^^^^^
  Total SOL        Tradeable SOL    Cumulative P&L
```

### Trade Log CSV

The bot writes every trade to `trades.csv` in the project directory:

```bash
# View recent trades
cat trades.csv

# Watch trades live
tail -f trades.csv
```

### Log Levels

Control verbosity with the `RUST_LOG` environment variable:

```bash
# Default (recommended)
RUST_LOG=solana_mev_bot=info cargo run --release

# Debug mode (very verbose - for troubleshooting)
RUST_LOG=solana_mev_bot=debug cargo run --release

# Quiet mode (only warnings and errors)
RUST_LOG=solana_mev_bot=warn cargo run --release
```

---

## Important Commands Reference

### Build Commands

| Command | What It Does |
|---------|-------------|
| `cargo build --release` | Compile the bot (optimized for speed) |
| `cargo build` | Compile in debug mode (faster compile, slower execution) |
| `cargo test` | Run all tests to verify code is working |
| `cargo run --release` | Build and run in one step |

### Solana CLI Commands

| Command | What It Does |
|---------|-------------|
| `solana balance` | Check your wallet's SOL balance |
| `solana balance <ADDRESS>` | Check any wallet's balance |
| `solana config set --keypair ~/mev-wallet.json` | Set default wallet |
| `solana config set --url https://api.mainnet-beta.solana.com` | Set RPC URL |
| `solana config get` | Show current Solana CLI configuration |
| `solana-keygen pubkey ~/mev-wallet.json` | Show wallet public address |
| `solana transfer <ADDRESS> <AMOUNT>` | Send SOL to another address |
| `solana confirm <TX_SIGNATURE>` | Check if a transaction was confirmed |

### Running the Bot

| Command | What It Does |
|---------|-------------|
| `cargo run --release` | Run bot in foreground |
| `cargo run --release > bot.log 2>&1 &` | Run in background, log to file |
| `tail -f bot.log` | Watch log output live |
| `screen -S mevbot` then `cargo run --release` | Run in a screen session |
| `screen -r mevbot` | Reattach to screen session |
| `Ctrl+C` | Stop the bot (foreground) |
| `kill $(pgrep solana-mev)` | Stop the bot (background) |

### Checking Results

| Command | What It Does |
|---------|-------------|
| `cat trades.csv` | View all executed trades |
| `tail -20 trades.csv` | View last 20 trades |
| `wc -l trades.csv` | Count total trades |
| `solana balance` | Check current wallet balance |

---

## Troubleshooting

### "Connection refused" or RPC errors

```
CAUSE: Bad RPC URL or rate limiting
FIX:
  1. Verify your RPC_URL is correct in .env
  2. Check your API key hasn't expired
  3. If using free tier, you may be rate limited - upgrade your plan
  4. Try a different RPC provider
```

### "Insufficient balance" / bot skipping all trades

```
CAUSE: Wallet balance too low
FIX:
  1. Run: solana balance
  2. You need at least 0.1 SOL (0.05 SOL is held as reserve)
  3. Send more SOL to your wallet address
```

### Bot runs but finds zero opportunities

```
CAUSE: This is normal for well-arbitraged pairs
FIX:
  1. Add more pool addresses across more DEXes
  2. Try less common token pairs (they have more price discrepancies)
  3. Reduce MIN_PROFIT_SOL (but be careful of gas costs eating profits)
  4. Wait - opportunities are intermittent and depend on market activity
```

### "Simulation failed" on every trade

```
CAUSE: Pool state changed between detection and execution
FIX:
  1. Reduce LOOP_INTERVAL_MS (faster scanning)
  2. Enable Jito bundles (JITO_ENABLED=true) for priority execution
  3. Increase MAX_SLIPPAGE_PCT slightly (e.g., from 1.0 to 1.5)
  4. This is normal - other bots may be capturing the same opportunities
```

### Build errors

```bash
# Update Rust to latest version
rustup update

# Clean and rebuild
cargo clean
cargo build --release

# If dependency errors:
cargo update
cargo build --release
```

### Drawdown protection paused the bot

```
CAUSE: Balance dropped >20% from peak - safety mechanism activated
FIX:
  1. The bot will auto-resume when balance recovers
  2. Or restart the bot to reset the high-water mark
  3. Review your trades.csv to understand what went wrong
```

---

## Risk Warnings

1. **You can lose money.** Arbitrage is not risk-free. Transactions can fail after partial execution, pool states can change, and network congestion can cause slippage.

2. **Start small.** Begin with 0.5 SOL and demo mode. Only increase capital after you understand the bot's behavior.

3. **Private keys are everything.** If someone gets your private key, they can take all your SOL. Never share your `.env` file, never commit it to git, never paste your key anywhere online.

4. **RPC costs money.** Free RPC endpoints are too slow for competitive arbitrage. Budget for a paid RPC plan.

5. **Other bots are competing.** This is a competitive space. Many professional operations run similar strategies with faster infrastructure. Don't expect guaranteed profits.

6. **Gas costs eat into profits.** Every transaction costs SOL in fees. If your trades are too small, fees may exceed profits.

7. **Smart contract risk.** The bot interacts with third-party DEX programs. Bugs in those programs could result in lost funds.

8. **Network risk.** Solana can experience congestion, outages, or degraded performance that affects trade execution.

**The bot defaults to demo mode (`ENABLE_REAL_EXECUTION=false`) for your protection. Only change this when you fully understand what you're doing.**
