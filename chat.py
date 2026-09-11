#!/usr/bin/env python3
import urllib.request
import json
import sys

URL = "http://localhost:8080/human-brain/chat"

print("==========================================================================")
print("  🦀 LIVE INTERACTIVE CHAT: REALSSA RUST BOT ENGINE")
print("  Type your message and press ENTER. Type 'exit' or 'quit' to stop.")
print("==========================================================================\n")

while True:
    try:
        user_input = input("You > ").strip()
        if not user_input:
            continue
        if user_input.lower() in ["exit", "quit", "q"]:
            print("\nGoodbye! RealSSA Rust Engine memory saved.")
            break
            
        req = urllib.request.Request(
            URL,
            data=json.dumps({"message": user_input}).encode("utf-8"),
            headers={"Content-Type": "application/json"}
        )
        
        with urllib.request.urlopen(req, timeout=30) as response:
            res_data = json.loads(response.read().decode("utf-8"))
            reply = res_data.get("reply", "No response")
            insights = res_data.get("total_insights", 0)
            print(f"\nRust Bot (Insights: {insights}) >\n{reply}\n")
            
    except KeyboardInterrupt:
        print("\nChat ended.")
        break
    except Exception as e:
        print(f"\n[Error connecting to Rust Engine on port 8080]: {e}\n")
