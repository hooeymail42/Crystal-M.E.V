#!/usr/bin/env python3
"""
AI Risk Management Dashboard
Real-time monitoring of AI risk management system
"""

import json
import time
import os
from datetime import datetime
import threading

class AIDashboard:
    def __init__(self):
        self.risk_manager = None
        self.running = False
        
    def load_risk_state(self):
        """Load current risk state"""
        try:
            with open('ai_risk_state.json', 'r') as f:
                return json.load(f)
        except FileNotFoundError:
            return None
    
    def display_dashboard(self):
        """Display real-time dashboard"""
        while self.running:
            os.system('clear')
            print("🤖 AI RISK MANAGEMENT DASHBOARD")
            print("================================")
            print(f"Last Update: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}")
            print()
            
            state = self.load_risk_state()
            if state:
                perf = state.get('performance', {})
                params = state.get('risk_parameters', {})
                
                total_trades = perf.get('total_trades', 0)
                profitable = perf.get('profitable_trades', 0)
                win_rate = (profitable / total_trades * 100) if total_trades > 0 else 0
                
                print(f"Total Trades: {total_trades}")
                print(f"Win Rate: {win_rate:.1f}%")
                print(f"Current Streak: {perf.get('current_streak', 0)}")
                print(f"Patterns Learned: {len(state.get('pattern_weights', {}))}")
                print()
                print("Risk Parameters:")
                print(f"  Max Position: {params.get('max_position_size', 0):.3f} SOL")
                print(f"  Min Profit: {params.get('min_profit_threshold', 0):.4f} SOL")
                print(f"  Confidence: {params.get('confidence_threshold', 0):.2f}")
            else:
                print("No risk state found. AI system not yet active.")
                
            print()
            print("Press Ctrl+C to exit")
            time.sleep(2)
    
    def start(self):
        """Start the dashboard"""
        self.running = True
        try:
            self.display_dashboard()
        except KeyboardInterrupt:
            self.running = False
            print("\nDashboard stopped.")

if __name__ == "__main__":
    dashboard = AIDashboard()
    dashboard.start()
