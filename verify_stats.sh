#!/bin/bash
set -e

# Performance Verification Sweep
# This script runs the runtime benchmark at various concurrency levels
# to verify the stats for the resume/README.

echo "🚀 Building Async Runtime in Release Mode..."
cargo build --release

# Configurable Sweep Parameters
# (Concurrency levels, Total Messages per level, Payload Size)
levels=(100 500 1000 5000 10000)
messages=(1000000 1000000 1000000 1000000 1000000)
payload=1024

echo "⏱️ Starting Performance Sweep..."
echo "=================================================="

for i in "${!levels[@]}"; do
    c=${levels[$i]}
    m=${messages[$i]}
    
    echo "▶️ Testing: Concurrency=$c | Messages=$m | Payload=$payload"
    
    # Run the compiled binary directly to avoid cargo overhead
    ./target/release/runtime $c $m $payload --runs 10
    
    echo "⏸️  Sleeping 1s for port recovery..."
    sleep 1
    echo "--------------------------------------------------"
done

echo "✅ Performance Sweep Complete."
