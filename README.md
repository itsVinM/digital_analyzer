🔬 Digital Twin Environmental Analyzer

This repository contains the full stack for an embedded Digital Twin project focused on validating sensor readings and alarm logic in a multi-protocol embedded system.

The project uses a memory-safe, asynchronous approach (Rust) for the firmware and a high-level language (Python) for real-time visualization and remote control, mimicking a real-world IoT deployment.

🎯 Goals and Verification Focus

The primary goal is to establish robust Verification and Validation (V&V) platforms for safety-critical systems (simulated PSU monitoring).

Safety & Reliability: Implement drivers in Rust to ensure memory safety and deterministic alarm handling.

Digitalization: Create a real-time virtual representation (Digital Twin) of the physical system state via MQTT.

Testing Automation: Integrate simulation (Renode) and automated testing (Robot Framework) for comprehensive V&V of communication and alarm paths.