use std::env;

/// Configuration for Jito MEV infrastructure
#[derive(Debug, Clone)]
pub struct JitoConfig {
    pub enabled: bool,
    pub block_engine_url: String,
    pub tip_account: String,
    pub searcher_private_key: Option<String>,
    pub min_profitable_lamports: u64,
}

impl JitoConfig {
    /// Load Jito configuration from environment
    pub fn load() -> Self {
        let enabled = env::var("JITO_ENABLED")
            .map(|v| v.to_lowercase() == "true")
            .unwrap_or(false);

        let block_engine_url = env::var("JITO_BLOCK_ENGINE_URL")
            .unwrap_or_else(|_| "https://mainnet.block-engine.jito.wtf".to_string());

        let tip_account = env::var("JITO_TIP_ACCOUNT")
            .unwrap_or_else(|_| "Eo1nUHVV8qJzQ9p6aNxZ4CJxXzEdLdCVLDEFNzKKJH8j".to_string());

        let searcher_private_key = env::var("JITO_SEARCHER_PRIVATE_KEY").ok();

        let min_profitable_lamports = env::var("JITO_MIN_PROFITABLE_LAMPORTS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100_000); // 0.0001 SOL minimum

        Self {
            enabled,
            block_engine_url,
            tip_account,
            searcher_private_key,
            min_profitable_lamports,
        }
    }

    /// Validate configuration
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }

        if self.block_engine_url.is_empty() {
            return Err("JITO_BLOCK_ENGINE_URL is required when Jito is enabled".to_string());
        }

        if self.tip_account.is_empty() {
            return Err("JITO_TIP_ACCOUNT is required when Jito is enabled".to_string());
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jito_config_default() {
        let config = JitoConfig::load();
        assert!(!config.enabled); // Default disabled
        assert!(!config.block_engine_url.is_empty());
    }
}
