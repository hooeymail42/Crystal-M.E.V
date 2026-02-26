#!/usr/bin/env python3
import subprocess
import os
import sys
import time

class MEVBotSetupAgent:
    def __init__(self):
        self.project_dir = os.getcwd()
        
    def run_command(self, command, description=""):
        """Run shell command with error handling"""
        print(f"🚀 {description}...")
        try:
            result = subprocess.run(command, shell=True, capture_output=True, text=True)
            if result.returncode == 0:
                print(f"✅ {description} - Success")
                return True
            else:
                print(f"❌ {description} - Failed: {result.stderr}")
                return False
        except Exception as e:
            print(f"❌ {description} - Error: {e}")
            return False
    
    def setup_environment(self):
        """Setup Rust and Solana environment"""
        commands = [
            ("source $HOME/.cargo/env", "Loading Rust environment"),
            ("rustc --version", "Checking Rust installation"),
            ("solana --version", "Checking Solana CLI"),
            ("cargo --version", "Checking Cargo")
        ]
        
        for cmd, desc in commands:
            if not self.run_command(cmd, desc):
                return False
        return True
    
    def build_bot(self):
        """Build the MEV bot"""
        return self.run_command("cargo build --release", "Building MEV bot")
    
    def setup_wallet(self):
        """Setup test wallet"""
        commands = [
            ("solana-keygen new --outfile ./test-wallet.json --no-passphrase", "Creating test wallet"),
            ("solana airdrop 1 $(solana address -k ./test-wallet.json) --url devnet", "Funding wallet with devnet SOL")
        ]
        
        for cmd, desc in commands:
            if not self.run_command(cmd, desc):
                print("⚠️  Airdrop might be rate limited - continuing...")
        return True
    
    def setup_config(self):
        """Setup configuration files"""
        # Create .env file
        env_content = """RPC_URL=https://api.devnet.solana.com
WALLET_PRIVATE_KEY=UPDATE_WITH_YOUR_KEY
BOT_COMPUTE_UNIT_LIMIT=600000
MINT_1=So11111111111111111111111111111111111111112
MINT_1_PROCESS_DELAY=1000
"""
        
        with open("../.env", "w") as f:
            f.write(env_content)
        
        print("✅ Created .env configuration")
        return True
    
    def install_python_dependencies(self):
        """Install required Python packages for agents"""
        packages = ["requests", "toml", "python-telegram-bot"]
        for package in packages:
            self.run_command(f"pip3 install {package}", f"Installing {package}")
        return True
    
    def full_setup(self):
        """Execute full setup process"""
        print("🤖 Starting MEV Bot Agent Setup...")
        
        steps = [
            ("Environment Setup", self.setup_environment),
            ("Python Dependencies", self.install_python_dependencies),
            ("Wallet Setup", self.setup_wallet),
            ("Configuration Setup", self.setup_config),
            ("Bot Compilation", self.build_bot)
        ]
        
        for step_name, step_function in steps:
            print(f"\n{'='*50}")
            print(f"Step: {step_name}")
            print(f"{'='*50}")
            if not step_function():
                print(f"❌ Setup failed at: {step_name}")
                return False
        
        print(f"\n{'='*50}")
        print("🎉 MEV Bot Agent Setup Complete!")
        print("Next steps:")
        print("1. Update .env with your wallet private key")
        print("2. Run: python3 agent/scripts/health_monitor.py")
        print("3. Monitor logs in agent/logs/")
        print(f"{'='*50}")
        return True

if __name__ == "__main__":
    agent = MEVBotSetupAgent()
    agent.full_setup()
