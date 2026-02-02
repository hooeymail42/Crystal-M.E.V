use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use chrono::Utc;
use tracing::{info, warn, error, debug};
use futures::stream::StreamExt;
use futures::SinkExt;
use tokio_tungstenite::tungstenite::Message;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PoolStateUpdate {
    pub pool_address: Pubkey,
    pub token_a: Pubkey,
    pub token_b: Pubkey,
    pub reserve_a: u128,
    pub reserve_b: u128,
    pub price: f64,
    pub liquidity: f64,
    pub timestamp: i64,
    pub slot: u64,
}

/// Raw account update received from the WebSocket before DEX-specific parsing.
#[derive(Clone, Debug)]
pub struct RawAccountUpdate {
    pub pool_address: Pubkey,
    pub data: Vec<u8>,
    pub slot: u64,
    pub dex_name: String,
}

// ---------------------------------------------------------------------------
// PoolSubscriptionManager  (in-memory cache, unchanged public API)
// ---------------------------------------------------------------------------

pub struct PoolSubscriptionManager {
    pools: Arc<RwLock<HashMap<Pubkey, PoolStateUpdate>>>,
    update_tx: mpsc::UnboundedSender<PoolStateUpdate>,
    update_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<PoolStateUpdate>>>,
}

impl PoolSubscriptionManager {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();

        Self {
            pools: Arc::new(RwLock::new(HashMap::new())),
            update_tx: tx,
            update_rx: Arc::new(tokio::sync::Mutex::new(rx)),
        }
    }

    pub async fn get_pool_state(&self, pool_address: Pubkey) -> Result<PoolStateUpdate> {
        let pools = self.pools.read().await;
        pools.get(&pool_address)
            .cloned()
            .ok_or_else(|| anyhow!("Pool not found or not subscribed: {}", pool_address))
    }

    pub async fn get_all_pools(&self) -> Result<Vec<PoolStateUpdate>> {
        let pools = self.pools.read().await;
        Ok(pools.values().cloned().collect())
    }

    pub async fn update_pool_state(&self, update: PoolStateUpdate) -> Result<()> {
        let mut pools = self.pools.write().await;
        pools.insert(update.pool_address, update.clone());
        let _ = self.update_tx.send(update);
        Ok(())
    }

    pub async fn get_recent_updates(&self, seconds: i64) -> Result<Vec<PoolStateUpdate>> {
        let now = Utc::now().timestamp();
        let pools = self.pools.read().await;

        let recent = pools
            .values()
            .filter(|p| (now - p.timestamp) <= seconds)
            .cloned()
            .collect();

        Ok(recent)
    }

    pub async fn print_summary(&self) -> Result<()> {
        let pools = self.pools.read().await;
        let total_liquidity: f64 = pools.values().map(|p| p.liquidity).sum();

        info!("Pool Subscription Summary: pools={}, total_liquidity={:.2}", pools.len(), total_liquidity);

        for (idx, pool) in pools.values().enumerate().take(5) {
            info!(
                "  {}. {} - Price: {:.6}, Liquidity: {:.2}",
                idx + 1, pool.pool_address, pool.price, pool.liquidity
            );
        }

        if pools.len() > 5 {
            info!("  ... and {} more pools", pools.len() - 5);
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// WebSocket subscriber
// ---------------------------------------------------------------------------

/// Subscribes to on-chain pool account changes over WebSocket and forwards
/// raw account data through an mpsc channel so the caller can deserialize
/// with the appropriate DEX-specific logic (reusing `PoolRefreshManager`).
pub struct WebSocketPoolSubscriber {
    ws_url: String,
    /// Maps pool Pubkey -> (subscription_request_id, dex_name)
    pool_subscriptions: Vec<(Pubkey, String)>,
}

impl WebSocketPoolSubscriber {
    pub fn new(ws_url: String) -> Self {
        Self {
            ws_url,
            pool_subscriptions: Vec::new(),
        }
    }

    /// Register a pool address to subscribe to.
    pub fn add_pool(&mut self, pool_address: Pubkey, dex_name: String) {
        self.pool_subscriptions.push((pool_address, dex_name));
    }

    /// Register many pool addresses at once.
    pub fn add_pools(&mut self, pools: Vec<(Pubkey, String)>) {
        self.pool_subscriptions.extend(pools);
    }

    /// Number of registered pool subscriptions.
    pub fn pool_count(&self) -> usize {
        self.pool_subscriptions.len()
    }
}

/// Start the WebSocket subscriber. Returns a receiver of `RawAccountUpdate`.
///
/// This spawns a long-lived tokio task that:
/// 1. Connects to the Solana WebSocket RPC.
/// 2. Sends `accountSubscribe` for every registered pool address.
/// 3. Forwards decoded account data through the channel.
/// 4. Reconnects with exponential backoff on disconnect.
pub fn start_websocket_subscriber(
    subscriber: WebSocketPoolSubscriber,
) -> mpsc::UnboundedReceiver<RawAccountUpdate> {
    let (tx, rx) = mpsc::unbounded_channel();

    tokio::spawn(ws_connection_loop(
        subscriber.ws_url,
        subscriber.pool_subscriptions,
        tx,
    ));

    rx
}

async fn ws_connection_loop(
    ws_url: String,
    pool_subs: Vec<(Pubkey, String)>,
    tx: mpsc::UnboundedSender<RawAccountUpdate>,
) {
    let mut backoff_ms: u64 = 500;
    const MAX_BACKOFF_MS: u64 = 30_000;

    loop {
        info!("WebSocket: connecting to {} ({} pools)", ws_url, pool_subs.len());

        match tokio_tungstenite::connect_async(&ws_url).await {
            Ok((ws_stream, _response)) => {
                info!("WebSocket: connected");
                backoff_ms = 500; // reset backoff on successful connect

                let (mut write, mut read) = ws_stream.split();

                // Maps JSON-RPC request id -> (pool_address, dex_name)
                let mut request_id_map: HashMap<u64, (Pubkey, String)> = HashMap::new();
                // Maps subscription_id (returned by server) -> (pool_address, dex_name)
                let mut sub_id_map: HashMap<u64, (Pubkey, String)> = HashMap::new();

                // Send accountSubscribe for each pool
                for (idx, (pool_addr, dex_name)) in pool_subs.iter().enumerate() {
                    let request_id = (idx + 1) as u64;
                    let subscribe_msg = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "method": "accountSubscribe",
                        "params": [
                            pool_addr.to_string(),
                            {
                                "encoding": "base64",
                                "commitment": "confirmed"
                            }
                        ]
                    });

                    request_id_map.insert(request_id, (*pool_addr, dex_name.clone()));

                    if let Err(e) = write.send(Message::Text(subscribe_msg.to_string())).await {
                        error!("WebSocket: failed to send subscribe for {}: {}", pool_addr, e);
                        break;
                    }
                }

                debug!("WebSocket: sent {} accountSubscribe requests", pool_subs.len());

                // Read loop
                let mut disconnected = false;
                while let Some(msg_result) = read.next().await {
                    match msg_result {
                        Ok(Message::Text(text)) => {
                            if let Err(e) = handle_ws_message(
                                &text,
                                &request_id_map,
                                &mut sub_id_map,
                                &tx,
                            ) {
                                debug!("WebSocket: message handling error: {}", e);
                            }
                        }
                        Ok(Message::Ping(data)) => {
                            let _ = write.send(Message::Pong(data)).await;
                        }
                        Ok(Message::Close(_)) => {
                            warn!("WebSocket: server sent close frame");
                            disconnected = true;
                            break;
                        }
                        Err(e) => {
                            warn!("WebSocket: read error: {}", e);
                            disconnected = true;
                            break;
                        }
                        _ => {}
                    }
                }

                if !disconnected {
                    warn!("WebSocket: stream ended unexpectedly");
                }
            }
            Err(e) => {
                warn!("WebSocket: connection failed: {}", e);
            }
        }

        // Exponential backoff before reconnect
        warn!("WebSocket: reconnecting in {}ms", backoff_ms);
        tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
        backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
    }
}

/// Parse a single WebSocket text message.
///
/// There are two kinds of messages we expect:
/// 1. Subscription confirmation: `{"jsonrpc":"2.0","result":<sub_id>,"id":<req_id>}`
///    We use this to map the subscription id back to the pool address.
/// 2. Account notification: `{"jsonrpc":"2.0","method":"accountNotification","params":{...}}`
///    We decode the base64 account data and send it through the channel.
fn handle_ws_message(
    text: &str,
    request_id_map: &HashMap<u64, (Pubkey, String)>,
    sub_id_map: &mut HashMap<u64, (Pubkey, String)>,
    tx: &mpsc::UnboundedSender<RawAccountUpdate>,
) -> Result<()> {
    let v: serde_json::Value = serde_json::from_str(text)?;

    // Case 1: subscription confirmation
    if let (Some(id), Some(result)) = (v.get("id"), v.get("result")) {
        if let (Some(req_id), Some(sub_id)) = (id.as_u64(), result.as_u64()) {
            if let Some((pool_addr, dex_name)) = request_id_map.get(&req_id) {
                sub_id_map.insert(sub_id, (*pool_addr, dex_name.clone()));
                debug!("WebSocket: subscribed to {} (sub_id={})", pool_addr, sub_id);
            }
        }
        return Ok(());
    }

    // Case 2: account notification
    if v.get("method").and_then(|m| m.as_str()) == Some("accountNotification") {
        let params = v.get("params").ok_or_else(|| anyhow!("missing params"))?;
        let subscription = params.get("subscription")
            .and_then(|s| s.as_u64())
            .ok_or_else(|| anyhow!("missing subscription id"))?;

        let (pool_addr, dex_name) = sub_id_map.get(&subscription)
            .cloned()
            .ok_or_else(|| anyhow!("unknown subscription id {}", subscription))?;

        let result = params.get("result")
            .ok_or_else(|| anyhow!("missing result"))?;

        let slot = result.get("context")
            .and_then(|c| c.get("slot"))
            .and_then(|s| s.as_u64())
            .unwrap_or(0);

        let account_data = result.get("value")
            .and_then(|v| v.get("data"))
            .ok_or_else(|| anyhow!("missing account data"))?;

        // data is [base64_string, "base64"]
        let base64_str = if let Some(arr) = account_data.as_array() {
            arr.first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("data array missing base64 string"))?
        } else if let Some(s) = account_data.as_str() {
            s
        } else {
            return Err(anyhow!("unexpected data format"));
        };

        use base64::Engine;
        let decoded = base64::engine::general_purpose::STANDARD.decode(base64_str)
            .map_err(|e| anyhow!("base64 decode failed: {}", e))?;

        let update = RawAccountUpdate {
            pool_address: pool_addr,
            data: decoded,
            slot,
            dex_name,
        };

        let _ = tx.send(update);

        return Ok(());
    }

    // Case 3: error response
    if let Some(err) = v.get("error") {
        warn!("WebSocket: RPC error: {}", err);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Cache (preserved from original)
// ---------------------------------------------------------------------------

pub struct PoolStateCache {
    cache: Arc<RwLock<HashMap<Pubkey, CachedPoolState>>>,
    ttl_seconds: u64,
}

#[derive(Clone)]
struct CachedPoolState {
    state: PoolStateUpdate,
    cached_at: i64,
}

impl PoolStateCache {
    pub fn new(ttl_seconds: u64) -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
            ttl_seconds,
        }
    }

    pub async fn get(&self, pool: Pubkey) -> Option<PoolStateUpdate> {
        let cache = self.cache.read().await;

        if let Some(cached) = cache.get(&pool) {
            let age = Utc::now().timestamp() - cached.cached_at;
            if age <= self.ttl_seconds as i64 {
                return Some(cached.state.clone());
            }
        }

        None
    }

    pub async fn set(&self, state: PoolStateUpdate) {
        let mut cache = self.cache.write().await;
        cache.insert(state.pool_address, CachedPoolState {
            state,
            cached_at: Utc::now().timestamp(),
        });
    }

    pub async fn clear(&self) {
        let mut cache = self.cache.write().await;
        cache.clear();
    }

    pub async fn get_stats(&self) -> CacheStats {
        let cache = self.cache.read().await;
        let now = Utc::now().timestamp();

        let valid_entries = cache.values()
            .filter(|e| (now - e.cached_at) <= self.ttl_seconds as i64)
            .count();

        CacheStats {
            total_entries: cache.len(),
            valid_entries,
            expired_entries: cache.len() - valid_entries,
            ttl_seconds: self.ttl_seconds,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CacheStats {
    pub total_entries: usize,
    pub valid_entries: usize,
    pub expired_entries: usize,
    pub ttl_seconds: u64,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_cache() {
        let cache = PoolStateCache::new(60);

        let update = PoolStateUpdate {
            pool_address: Pubkey::default(),
            token_a: Pubkey::default(),
            token_b: Pubkey::default(),
            reserve_a: 1000,
            reserve_b: 2000,
            price: 0.5,
            liquidity: 3000.0,
            timestamp: Utc::now().timestamp(),
            slot: 0,
        };

        cache.set(update.clone()).await;
        let retrieved = cache.get(update.pool_address).await;
        assert!(retrieved.is_some());
    }

    #[tokio::test]
    async fn test_subscription_manager_update() {
        let mgr = PoolSubscriptionManager::new();

        let update = PoolStateUpdate {
            pool_address: Pubkey::new_unique(),
            token_a: Pubkey::default(),
            token_b: Pubkey::default(),
            reserve_a: 500,
            reserve_b: 1000,
            price: 0.5,
            liquidity: 1500.0,
            timestamp: Utc::now().timestamp(),
            slot: 42,
        };

        mgr.update_pool_state(update.clone()).await.unwrap();
        let retrieved = mgr.get_pool_state(update.pool_address).await.unwrap();
        assert_eq!(retrieved.slot, 42);
    }

    #[test]
    fn test_websocket_subscriber_construction() {
        let mut sub = WebSocketPoolSubscriber::new("wss://example.com".to_string());
        sub.add_pool(Pubkey::new_unique(), "Raydium".to_string());
        sub.add_pools(vec![
            (Pubkey::new_unique(), "Pump".to_string()),
            (Pubkey::new_unique(), "DLMM".to_string()),
        ]);
        assert_eq!(sub.pool_subscriptions.len(), 3);
    }

    #[test]
    fn test_handle_subscription_confirmation() {
        let pool = Pubkey::new_unique();
        let mut request_map = HashMap::new();
        request_map.insert(1u64, (pool, "Raydium".to_string()));
        let mut sub_map = HashMap::new();
        let (tx, _rx) = mpsc::unbounded_channel();

        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "result": 12345,
            "id": 1
        });

        handle_ws_message(&msg.to_string(), &request_map, &mut sub_map, &tx).unwrap();
        assert!(sub_map.contains_key(&12345));
        assert_eq!(sub_map[&12345].0, pool);
    }

    #[test]
    fn test_handle_account_notification() {
        let pool = Pubkey::new_unique();
        let mut request_map = HashMap::new();
        let mut sub_map = HashMap::new();
        sub_map.insert(99u64, (pool, "Pump".to_string()));
        let (tx, mut rx) = mpsc::unbounded_channel();

        // Encode 100 bytes of zeros as base64
        use base64::Engine;
        let data_bytes = vec![0u8; 100];
        let b64 = base64::engine::general_purpose::STANDARD.encode(&data_bytes);

        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "accountNotification",
            "params": {
                "subscription": 99,
                "result": {
                    "context": { "slot": 12345 },
                    "value": {
                        "data": [b64, "base64"],
                        "executable": false,
                        "lamports": 1000000,
                        "owner": "11111111111111111111111111111111",
                        "rentEpoch": 0
                    }
                }
            }
        });

        handle_ws_message(&msg.to_string(), &request_map, &mut sub_map, &tx).unwrap();

        let update = rx.try_recv().unwrap();
        assert_eq!(update.pool_address, pool);
        assert_eq!(update.slot, 12345);
        assert_eq!(update.dex_name, "Pump");
        assert_eq!(update.data.len(), 100);
    }
}
