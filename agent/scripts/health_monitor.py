#!/usr/bin/env python3
import json
import subprocess
import time
import requests
from datetime import datetime

class MEVBotMonitor:
    def __init__(self, config_path):
        self.config = self.load_config(config_path)
        self.bot_process = None
        
    def load_config(self, path):
        with open(path, 'r') as f:
            return json.load(f)
    
    def check_bot_status(self):
        """Check if MEV bot process is running"""
        try:
            result = subprocess.run(
                ["pgrep", "-f", "solana-mev-bot"],
                capture_output=True,
                text=True
            )
            return len(result.stdout.strip().split('\n')) > 0
        except Exception as e:
            print(f"Error checking bot status: {e}")
            return False
    
    def check_wallet_balance(self):
        """Check wallet balance"""
        try:
            result = subprocess.run(
                ["solana", "balance", "--url", "devnet"],
                capture_output=True,
                text=True
            )
            return result.stdout.strip()
        except Exception as e:
            return f"Error: {e}"
    
    def restart_bot(self):
        """Restart the MEV bot"""
        try:
            # Kill existing processes
            subprocess.run(["pkill", "-f", "solana-mev-bot"])
            time.sleep(2)
            
            # Start new process
            self.bot_process = subprocess.Popen(
                ["./target/release/solana-mev-bot"],
                cwd="..",
                stdout=open("agent/logs/bot_output.log", "a"),
                stderr=subprocess.STDOUT
            )
            return True
        except Exception as e:
            print(f"Error restarting bot: {e}")
            return False
    
    def monitor_loop(self):
        """Main monitoring loop"""
        while True:
            try:
                timestamp = datetime.now().isoformat()
                is_running = self.check_bot_status()
                balance = self.check_wallet_balance()
                
                print(f"[{timestamp}] Bot running: {is_running}, Balance: {balance}")
                
                if not is_running and self.config['trading']['auto_restart_on_crash']:
                    print("Bot not running - restarting...")
                    self.restart_bot()
                
                time.sleep(self.config['monitoring']['check_interval_seconds'])
                
            except Exception as e:
                print(f"Monitoring error: {e}")
                time.sleep(60)

if __name__ == "__main__":
    monitor = MEVBotMonitor("agent/config/agent_config.json")
    monitor.monitor_loop()
