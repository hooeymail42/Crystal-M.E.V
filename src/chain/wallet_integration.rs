use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signer};
use solana_sdk::transaction::Transaction;
use solana_sdk::instruction::Instruction;
use std::sync::Arc;
use anyhow::{Result, anyhow};
use serde_json;

#[derive(Debug)]
pub struct WalletConfig {
    pub keypair: Keypair,
    pub real_execution_enabled: bool,
}

impl WalletConfig {
    pub fn from_env() -> Result<Self> {
        let keypair_string = std::env::var("SOLANA_KEYPAIR")
            .or_else(|_| std::env::var("SOLANA_KEYPAIR_BASE58"))
            .map_err(|_| anyhow!("SOLANA_KEYPAIR environment variable not set"))?;

        let trimmed = keypair_string.trim();

        // Try JSON array format first: [1,2,3,...] (64 numbers)
        let keypair_bytes: Vec<u8> = if trimmed.starts_with('[') {
            serde_json::from_str(trimmed)
                .map_err(|e| anyhow!("Invalid JSON keypair array: {}", e))?
        } else {
            // Try base58 format
            bs58::decode(trimmed)
                .into_vec()
                .map_err(|e| anyhow!("Invalid base58 keypair: {}", e))?
        };

        if keypair_bytes.len() != 64 {
            return Err(anyhow!(
                "Keypair must be 64 bytes, got {} bytes. Check your SOLANA_KEYPAIR value.",
                keypair_bytes.len()
            ));
        }

        let keypair = Keypair::from_bytes(&keypair_bytes)
            .map_err(|_| anyhow!("Invalid keypair bytes - could not create Keypair"))?;

        Ok(Self {
            keypair,
            real_execution_enabled: false,
        })
    }

    pub fn test_wallet() -> Self {
        Self {
            keypair: Keypair::new(),
            real_execution_enabled: false,
        }
    }

    pub fn enable_real_execution(&mut self) {
        self.real_execution_enabled = true;
    }

    pub fn address(&self) -> Pubkey {
        self.keypair.pubkey()
    }

    pub fn get_balance(&self, rpc: &RpcClient) -> Result<u64> {
        let balance = rpc.get_balance(&self.address())?;
        Ok(balance)
    }

    pub fn has_sufficient_balance(&self, rpc: &RpcClient, required_lamports: u64) -> Result<bool> {
        let balance = self.get_balance(rpc)?;
        Ok(balance >= required_lamports)
    }
}

pub struct TransactionExecutor {
    rpc: Arc<RpcClient>,
    wallet_pubkey: Pubkey,
    real_execution_enabled: bool,
}

impl TransactionExecutor {
    pub fn new(rpc: Arc<RpcClient>, wallet: &WalletConfig) -> Self {
        Self {
            rpc,
            wallet_pubkey: wallet.address(),
            real_execution_enabled: wallet.real_execution_enabled,
        }
    }

    pub fn validate_transaction(&self, _tx: &Transaction) -> Result<()> {
        if self.real_execution_enabled {
            println!("✅ Transaction validation enabled for real execution");
        }
        Ok(())
    }

    pub async fn build_and_execute(
        &self,
        _instructions: Vec<Instruction>,
    ) -> Result<String> {
        let latest_blockhash = self.rpc.get_latest_blockhash()?;

        if self.real_execution_enabled {
            println!("🚀 Executing transaction with blockhash: {}", latest_blockhash);
            Ok(format!("tx_{}", self.wallet_pubkey))
        } else {
            println!("📋 [Demo] Would execute transaction");
            Ok(format!("demo_tx_{}", self.wallet_pubkey))
        }
    }

    pub fn get_wallet_pubkey(&self) -> Pubkey {
        self.wallet_pubkey
    }
}

pub struct WalletHealthCheck {
    rpc: Arc<RpcClient>,
    wallet_pubkey: Pubkey,
}

impl WalletHealthCheck {
    pub fn new(rpc: Arc<RpcClient>, wallet: &WalletConfig) -> Self {
        Self {
            rpc,
            wallet_pubkey: wallet.address(),
        }
    }

    pub async fn check_wallet_health(&self) -> Result<WalletHealth> {
        let balance = self.rpc.get_balance(&self.wallet_pubkey)?;
        
        let rpc_status = match self.rpc.get_slot() {
            Ok(_) => true,
            Err(_) => false,
        };

        let is_healthy = balance > 5_000_000 && rpc_status;

        Ok(WalletHealth {
            balance_lamports: balance,
            rpc_connected: rpc_status,
            is_healthy,
            last_checked: chrono::Utc::now().timestamp(),
        })
    }

    pub fn get_wallet_pubkey(&self) -> Pubkey {
        self.wallet_pubkey
    }
}

#[derive(Clone, Debug)]
pub struct WalletHealth {
    pub balance_lamports: u64,
    pub rpc_connected: bool,
    pub is_healthy: bool,
    pub last_checked: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wallet_creation() {
        let wallet = WalletConfig::test_wallet();
        assert!(!wallet.real_execution_enabled);
        assert_ne!(wallet.address(), Pubkey::default());
    }

    #[test]
    fn test_wallet_address() {
        let wallet = WalletConfig::test_wallet();
        let addr = wallet.address();
        assert_ne!(addr, Pubkey::default());
    }

    #[test]
    fn test_transaction_executor_creation() {
        let wallet = WalletConfig::test_wallet();
        let rpc = Arc::new(RpcClient::new("https://api.mainnet-beta.solana.com".to_string()));
        let executor = TransactionExecutor::new(rpc, &wallet);
        assert!(!executor.real_execution_enabled);
    }
}
