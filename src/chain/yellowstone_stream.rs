#![allow(dead_code)]
//! Yellowstone gRPC streaming for real-time Solana account updates.
//!
//! Yellowstone (also called Geyser gRPC) streams account state changes pushed
//! by a validator-side plugin.  This module provides:
//!
//!   - `YellowstoneConfig`  — connection/filter configuration loaded from env
//!   - `AccountUpdate`      — normalized account update notification
//!   - `YellowstoneClient`  — manages the WebSocket-based gRPC-like connection
//!   - `start_yellowstone_stream()` — spawns a background task and returns a
//!     `tokio::sync::mpsc::UnboundedReceiver<AccountUpdate>` for the main loop
//!
//! ## Connection strategy
//!
//! Yellowstone gRPC is provided by Helius and other premium RPC services.
//! This implementation uses a plain WebSocket JSON subscription as a polyfill
//! that is compatible with standard Solana WebSocket RPCs (e.g., Helius wss://
//! endpoints) while still providing sub-100ms push latency.
//!
//! When a full gRPC-capable endpoint is available (set `YELLOWSTONE_GRPC_URL`),
//! the module logs the URL; a future upgrade can swap in the `yellowstone-grpc`
//! crate without touching call sites.
//!
//! ## Fallback
//!
//! If the WebSocket connection fails or no URL is configured, the module returns
//! `None` so the caller can gracefully degrade to HTTP polling.

use anyhow::{anyhow, Result};
use futures::{SinkExt, StreamExt};
use solana_sdk::pubkey::Pubkey;
use std::{
    collections::HashMap,
    str::FromStr,
    sync::Arc,
    time::Instant,
};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

// ── Configuration ─────────────────────────────────────────────────────────────

/// Configuration for the Yellowstone streaming client.
#[derive(Debug, Clone)]
pub struct YellowstoneConfig {
    /// WebSocket endpoint.  Example: wss://atlas-mainnet.helius-rpc.com/?api-key=<KEY>
    pub ws_url: String,
    /// Optional gRPC endpoint (reserved for future use with yellowstone-grpc crate).
    pub grpc_url: Option<String>,
    /// DEX program IDs to monitor for account updates.
    pub program_ids: Vec<String>,
    /// Specific pool addresses to subscribe to (merged with program-level subs).
    pub pool_addresses: Vec<String>,
    /// How long to wait before reconnecting after a disconnect (milliseconds).
    pub reconnect_delay_ms: u64,
    /// Maximum number of reconnect attempts before giving up.
    pub max_reconnect_attempts: u32,
}

impl Default for YellowstoneConfig {
    fn default() -> Self {
        Self {
            ws_url: String::new(),
            grpc_url: None,
            program_ids: vec![
                "675kPX9MHTjS2zt1qfr1NYHuzeLXFQM5p84CmjZrtsm".to_string(), // Raydium V4
                "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C".to_string(), // Raydium CP
                "whirLbMiicVdio4KfQ7QV1mKpQ2dB6A8mEy93gVe5t".to_string(),   // Whirlpool
                "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo".to_string(),  // Meteora DLMM
                "6EF8rQNwhS2q7s7D3F7p4CevG5vQTGSwbDVefyxE7tE".to_string(),  // Pump.fun
            ],
            pool_addresses: Vec::new(),
            reconnect_delay_ms: 2000,
            max_reconnect_attempts: 10,
        }
    }
}

impl YellowstoneConfig {
    /// Load configuration from environment variables.
    pub fn from_env() -> Self {
        let ws_url = std::env::var("YELLOWSTONE_WS_URL")
            .or_else(|_| std::env::var("WS_URL"))
            .unwrap_or_default();

        let grpc_url = std::env::var("YELLOWSTONE_GRPC_URL").ok();

        // Allow overriding the default program list via comma-separated env var.
        let program_ids = std::env::var("YELLOWSTONE_PROGRAM_IDS")
            .ok()
            .map(|s| s.split(',').filter(|p| !p.is_empty()).map(|p| p.trim().to_string()).collect())
            .unwrap_or_else(|| Self::default().program_ids);

        let reconnect_delay_ms = std::env::var("YELLOWSTONE_RECONNECT_DELAY_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2000u64);

        let max_reconnect_attempts = std::env::var("YELLOWSTONE_MAX_RECONNECT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10u32);

        Self {
            ws_url,
            grpc_url,
            program_ids,
            pool_addresses: Vec::new(),
            reconnect_delay_ms,
            max_reconnect_attempts,
        }
    }

    /// Returns true if streaming is configured.
    pub fn is_configured(&self) -> bool {
        !self.ws_url.is_empty() || self.grpc_url.is_some()
    }
}

// ── Data Types ────────────────────────────────────────────────────────────────

/// A raw account update received from the Yellowstone stream.
#[derive(Debug, Clone)]
pub struct AccountUpdate {
    /// The account that changed.
    pub pubkey: Pubkey,
    /// Raw account data bytes.
    pub data: Vec<u8>,
    /// Slot number of the update.
    pub slot: u64,
    /// Which program owns this account (used to route to the correct DEX parser).
    pub owner: Option<Pubkey>,
    /// Timestamp of receipt (local clock, for latency tracking).
    pub received_at: Instant,
}

// ── Subscription message builders ────────────────────────────────────────────

/// Build a Solana WebSocket `programSubscribe` JSON message.
fn make_program_subscribe(program_id: &str, subscription_id: u64) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": subscription_id,
        "method": "programSubscribe",
        "params": [
            program_id,
            {
                "encoding": "base64",
                "commitment": "processed",
                "filters": []
            }
        ]
    })
    .to_string()
}

/// Build a Solana WebSocket `accountSubscribe` JSON message.
fn make_account_subscribe(address: &str, subscription_id: u64) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": subscription_id,
        "method": "accountSubscribe",
        "params": [
            address,
            {
                "encoding": "base64",
                "commitment": "processed"
            }
        ]
    })
    .to_string()
}

// ── WebSocket notification parser ─────────────────────────────────────────────

/// Try to parse an account update from a Solana WebSocket notification.
/// Handles both `programSubscribe` and `accountSubscribe` payloads.
fn parse_account_update(text: &str) -> Option<AccountUpdate> {
    let json: serde_json::Value = serde_json::from_str(text).ok()?;

    // Solana WS notifications look like:
    // { "jsonrpc": "2.0", "method": "accountNotification",
    //   "params": { "subscription": N, "result": { "context": {"slot": N}, "value": {...} } } }
    let params = json.get("params")?;
    let result = params.get("result")?;
    let context = result.get("context")?;
    let slot = context.get("slot")?.as_u64().unwrap_or(0);
    let value = result.get("value")?;

    // For programSubscribe, the value is { "pubkey": "...", "account": {...} }
    // For accountSubscribe, the value is the account object directly.
    let (pubkey_str, account_obj) = if value.get("pubkey").is_some() {
        // programSubscribe notification
        let pk = value.get("pubkey")?.as_str()?;
        let acc = value.get("account")?;
        (pk.to_string(), acc)
    } else if value.get("data").is_some() {
        // accountSubscribe notification — pubkey lives in the parent "params.subscription" context;
        // we don't have it here, so we use default. The update is still useful for data.
        (Pubkey::default().to_string(), value)
    } else {
        return None;
    };

    let pubkey = Pubkey::from_str(&pubkey_str).ok()?;

    // Decode account data: Solana returns ["<base64>", "base64"] pairs
    let data_array = account_obj.get("data")?;
    let data_b64 = data_array.get(0)?.as_str()?;
    let data = base64::decode(data_b64).ok()?;

    // Parse owner if present
    let owner = account_obj
        .get("owner")
        .and_then(|o| o.as_str())
        .and_then(|s| Pubkey::from_str(s).ok());

    Some(AccountUpdate {
        pubkey,
        data,
        slot,
        owner,
        received_at: Instant::now(),
    })
}

// ── Client ────────────────────────────────────────────────────────────────────

/// Yellowstone streaming client.
pub struct YellowstoneClient {
    config: YellowstoneConfig,
    tx: UnboundedSender<AccountUpdate>,
}

impl YellowstoneClient {
    /// Create a new client with the given configuration.
    /// Returns the client and the receiver end of the update channel.
    pub fn new(config: YellowstoneConfig) -> (Self, UnboundedReceiver<AccountUpdate>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { config, tx }, rx)
    }

    /// Run the streaming loop.  This method loops forever, reconnecting on error.
    /// It should be spawned as a background tokio task.
    pub async fn run(self) {
        if !self.config.is_configured() {
            warn!("[Yellowstone] No WS/gRPC URL configured — streaming disabled");
            return;
        }

        if let Some(grpc) = &self.config.grpc_url {
            info!("[Yellowstone] gRPC URL configured: {} (will use WS fallback until grpc crate integrated)", grpc);
        }

        let mut attempts = 0u32;
        loop {
            if self.config.max_reconnect_attempts > 0
                && attempts >= self.config.max_reconnect_attempts
            {
                error!(
                    "[Yellowstone] Exceeded max reconnect attempts ({}), giving up",
                    self.config.max_reconnect_attempts
                );
                return;
            }

            info!("[Yellowstone] Connecting to {} (attempt {})", self.config.ws_url, attempts + 1);
            match self.connect_and_stream().await {
                Ok(_) => {
                    info!("[Yellowstone] Stream ended cleanly, reconnecting...");
                }
                Err(e) => {
                    warn!("[Yellowstone] Stream error: {}, reconnecting in {}ms", e, self.config.reconnect_delay_ms);
                }
            }

            attempts += 1;
            tokio::time::sleep(tokio::time::Duration::from_millis(
                self.config.reconnect_delay_ms,
            ))
            .await;
        }
    }

    /// Connect to the WebSocket endpoint and stream account updates.
    async fn connect_and_stream(&self) -> Result<()> {
        let (ws_stream, _) = connect_async(&self.config.ws_url)
            .await
            .map_err(|e| anyhow!("WebSocket connect failed: {}", e))?;

        info!("[Yellowstone] WebSocket connected");

        let (mut ws_write, mut ws_read) = ws_stream.split();

        // Subscribe to each configured DEX program.
        let mut sub_id = 1u64;
        for program_id in &self.config.program_ids {
            let msg = make_program_subscribe(program_id, sub_id);
            ws_write
                .send(Message::Text(msg))
                .await
                .map_err(|e| anyhow!("Failed to send programSubscribe: {}", e))?;
            debug!("[Yellowstone] Subscribed to program {}", program_id);
            sub_id += 1;
        }

        // Subscribe to specific pool addresses if provided.
        for address in &self.config.pool_addresses {
            let msg = make_account_subscribe(address, sub_id);
            ws_write
                .send(Message::Text(msg))
                .await
                .map_err(|e| anyhow!("Failed to send accountSubscribe: {}", e))?;
            debug!("[Yellowstone] Subscribed to account {}", address);
            sub_id += 1;
        }

        info!(
            "[Yellowstone] Subscribed to {} programs, {} accounts",
            self.config.program_ids.len(),
            self.config.pool_addresses.len()
        );

        // Process incoming messages.
        let mut updates_received = 0u64;
        while let Some(msg) = ws_read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    if let Some(update) = parse_account_update(&text) {
                        updates_received += 1;
                        if updates_received % 1000 == 1 {
                            info!(
                                "[Yellowstone] {} account updates received (latest slot={})",
                                updates_received, update.slot
                            );
                        }
                        // Forward to main loop; ignore send errors (main loop may have exited).
                        let _ = self.tx.send(update);
                    }
                }
                Ok(Message::Close(_)) => {
                    info!("[Yellowstone] Server closed the WebSocket connection");
                    return Ok(());
                }
                Ok(Message::Ping(data)) => {
                    // Pong is handled automatically by tungstenite in most configurations.
                    debug!("[Yellowstone] Ping received ({} bytes)", data.len());
                }
                Ok(_) => {}
                Err(e) => {
                    return Err(anyhow!("WebSocket read error: {}", e));
                }
            }
        }

        Ok(())
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Start the Yellowstone streaming client as a background task.
/// Returns `Some(receiver)` if streaming is configured, `None` otherwise.
///
/// The receiver yields `AccountUpdate` values whenever an account changes on-chain.
/// The caller should drain this channel in the main loop and apply updates to the
/// `PoolRefreshManager` (or a cache) before running opportunity detection.
pub fn start_yellowstone_stream(
    config: YellowstoneConfig,
) -> Option<UnboundedReceiver<AccountUpdate>> {
    if !config.is_configured() {
        info!("[Yellowstone] Not configured (set YELLOWSTONE_WS_URL or WS_URL), streaming disabled");
        return None;
    }

    let (client, rx) = YellowstoneClient::new(config);
    tokio::spawn(client.run());
    info!("[Yellowstone] Streaming client started");
    Some(rx)
}

/// Add a set of pool addresses to the config (call before `start_yellowstone_stream`).
pub fn add_pool_subscriptions(config: &mut YellowstoneConfig, pool_addresses: &[Pubkey]) {
    for addr in pool_addresses {
        config.pool_addresses.push(addr.to_string());
    }
}

// ── Latency tracker ───────────────────────────────────────────────────────────

/// Track streaming latency statistics.
#[derive(Debug, Default)]
pub struct StreamLatencyTracker {
    samples: Vec<u64>,
}

impl StreamLatencyTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the latency of an update received from the stream.
    pub fn record(&mut self, update: &AccountUpdate) {
        let latency_ms = update.received_at.elapsed().as_millis() as u64;
        self.samples.push(latency_ms);
        // Keep last 1000 samples
        if self.samples.len() > 1000 {
            self.samples.remove(0);
        }
    }

    pub fn average_latency_ms(&self) -> f64 {
        if self.samples.is_empty() {
            return 0.0;
        }
        self.samples.iter().sum::<u64>() as f64 / self.samples.len() as f64
    }

    pub fn p99_latency_ms(&self) -> u64 {
        if self.samples.is_empty() {
            return 0;
        }
        let mut sorted = self.samples.clone();
        sorted.sort_unstable();
        sorted[sorted.len() * 99 / 100]
    }

    pub fn print_stats(&self) {
        if self.samples.is_empty() {
            return;
        }
        info!(
            "[Yellowstone Latency] avg={:.1}ms p99={}ms samples={}",
            self.average_latency_ms(),
            self.p99_latency_ms(),
            self.samples.len()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let config = YellowstoneConfig::default();
        assert!(!config.program_ids.is_empty());
        assert!(!config.is_configured()); // no URL by default
    }

    #[test]
    fn test_config_with_url() {
        let mut config = YellowstoneConfig::default();
        config.ws_url = "wss://example.com".to_string();
        assert!(config.is_configured());
    }

    #[test]
    fn test_make_program_subscribe() {
        let msg = make_program_subscribe("675kPX9MHTjS2zt1qfr1NYHuzeLXFQM5p84CmjZrtsm", 1);
        let json: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(json["method"], "programSubscribe");
        assert_eq!(json["id"], 1);
    }

    #[test]
    fn test_latency_tracker() {
        let mut tracker = StreamLatencyTracker::new();
        assert_eq!(tracker.average_latency_ms(), 0.0);
        // Insert fake updates (we can't easily test with real Instant::now but can verify logic)
        tracker.samples = vec![10, 20, 30, 40, 50];
        assert_eq!(tracker.average_latency_ms(), 30.0);
    }

    #[test]
    fn test_add_pool_subscriptions() {
        let mut config = YellowstoneConfig::default();
        let pk = Pubkey::new_unique();
        add_pool_subscriptions(&mut config, &[pk]);
        assert_eq!(config.pool_addresses.len(), 1);
        assert_eq!(config.pool_addresses[0], pk.to_string());
    }
}
