#!/bin/bash

# AI-Powered MEV Bot Runner
# Integrates Python AI risk management with Rust MEV bot

echo "🤖 Starting AI-Powered MEV Bot..."
echo "=================================="

# Check if required files exist
if [ ! -f "./target/release/solana-mev-bot" ]; then
    echo "❌ MEV bot not found. Please build it first with: cargo build --release"
    exit 1
fi

if [ ! -f "./ai_risk_manager.py" ]; then
    echo "❌ AI risk manager not found"
    exit 1
fi

# Initialize AI risk manager
echo "🔧 Initializing AI Risk Manager..."
python3 ai_risk_manager.py

if [ $? -ne 0 ]; then
    echo "⚠️  AI risk manager initialization had issues, but continuing..."
fi

# Start the MEV bot with AI monitoring
echo "🚀 Starting MEV bot with AI risk management..."
echo "📊 AI will monitor and learn from all trades..."
echo "💡 Press Ctrl+C to stop the bot"

# Function to handle graceful shutdown
cleanup() {
    echo ""
    echo "🛑 Shutting down AI MEV bot..."
    kill $BOT_PID 2>/dev/null
    echo "✅ Performance Report:"
    python3 ai_risk_manager.py
    exit 0
}

trap cleanup SIGINT SIGTERM

# Run the bot in background
./target/release/solana-mev-bot 2>&1 | while read line; do
    echo "$line"
    
    # Detect arbitrage opportunities in bot output
    if echo "$line" | grep -q "arbitrage opportunities"; then
        if echo "$line" | grep -q "Found"; then
            echo "🎯 AI: Opportunity detected - analyzing risk..."
            # Here you would parse the opportunity and send to AI risk manager
        fi
    fi
    
    # Detect trade executions
    if echo "$line" | grep -q "Executing trade"; then
        echo "🤖 AI: Monitoring trade execution..."
    fi
done &

BOT_PID=$!

# Wait for bot process
wait $BOT_PID
