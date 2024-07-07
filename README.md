# Juggler

Juggler is an RPC wrapper designed to balance requests across multiple Ethereum nodes. It manages failures, desynchronizations, and timeouts, ensuring that requests are not sent to problematic nodes until they recover. This solution is ideal for handling the cost and complexity of maintaining Ethereum nodes, providing an efficient and resilient method for load balancing between nodes.

## Project Status

🚧 **Work in Progress** 🚧

This project is still under development and is not ready for production use. Several essential features are still being implemented and tested. Please keep this in mind before integrating it into critical systems.

## Table of Contents

- [Introduction](#introduction)
- [Motivation](#motivation)
- [Installation](#installation)
- [Usage](#usage)
- [Features](#features)
- [Examples](#examples)
- [Architecture](#architecture)
- [Contribution](#contribution)
- [License](#license)
- [Acknowledgements](#acknowledgements)

## Introduction

Juggler is a robust tool for managing multiple RPC providers, ensuring that requests are efficiently distributed among available nodes. This ensures greater service availability and resilience, mitigating common issues such as timeouts and synchronization failures.

## Motivation

Maintaining and operating Ethereum nodes can be costly and complex. Nodes can experience outages, desynchronizations, or simply prolonged timeouts. To mitigate these issues, Juggler offers a solution that:

- **Distributes Requests**: Balances requests across multiple nodes, ensuring no single node is overloaded.
- **Failure Handling**: Identifies problematic nodes and avoids sending requests to them until they recover.
- **Constant Monitoring**: Regular pings to check node status and reincorporate recovered nodes into the active pool.

## Installation (release WIP)

To install Juggler, add the dependency to your `Cargo.toml` file:

```toml
[dependencies]
juggler = "0.1.0"
```

## Usage

Below is a basic example of how to set up and use Juggler:

```rust
use alloy::providers::WsProvider;
use juggler::RpcBalancer;
use std::sync::Arc;
use tokio::sync::RwLock;

#[tokio::main]
async fn main() {
    let provider1 = Arc::new(WsProvider::new("ws://node1.example.com").await.unwrap());
    let provider2 = Arc::new(WsProvider::new("ws://node2.example.com").await.unwrap());

    let juggler = RpcBalancer::new(vec![provider1.clone(), provider2.clone()]);

    let result = juggler.execute(|provider| async move {
        provider.get_block_number().await.map_err(|e| e.into())
    }).await;

    match result {
        Ok(block_number) => println!("Current block number: {}", block_number),
        Err(err) => eprintln!("Error fetching block number: {}", err),
    }
}
```

## Features

- **Load Balancing**: Distributes requests across multiple nodes equitably.
- **Failure Handling**: Detects and avoids problematic nodes, reincorporating them once they recover.
- **Regular Pings**: Checks the status of nodes at regular intervals.
- **Easy to Use**: Simple and clear API for integration into existing projects.

## Examples

### Subscriptions

Juggler includes examples of how to subscribe to different events on the Ethereum blockchain:

- **Subscribe to Blocks**: Example of subscribing to new blocks.
- **Subscribe to Pending Transactions**: Example of subscribing to pending transactions.
- **Subscribe to Logs**: Example of subscribing to event logs.

To run these examples, use the following command:

```bash
cargo run --example <example_name>
```

For instance, to subscribe to pending transactions:

```bash
cargo run --example subscribe_pending_transactions
```

**To be implemented**:

- Examples of `execute` method.

## Architecture

Juggler is designed to be a resilient and efficient system for handling multiple RPC providers. The architecture consists of the following key components:

1. **Providers**: A `HashMap` of providers (WebSocket or IPC) managed through `Arc` and `RwLock` to ensure concurrent access.
2. **Load Balancer**: The balancer selects the next available provider in a round-robin fashion, ensuring equitable distribution of requests.
3. **Failure Handling**: Providers that fail to respond are moved to an unavailable list. These providers are periodically pinged to check if they have recovered.
4. **Regular Pings**: A defined time interval to check the status of unavailable providers, reincorporating them if they recover.

### Workflow

1. **Initialization**: Initialized with a list of providers and starts a regular ping process.
2. **Add Provider**: New providers can be added dynamically.
3. **Select Provider**: Selects the next available provider in a round-robin fashion.
4. **Execute Request**: Attempts to execute the request with the selected provider. If it fails, moves the provider to the unavailable list and tries the next provider.
5. **Subscriptions**: Allows subscribing to different events, handling reconnection automatically if a provider fails.

## Contribution

Contributions are welcome! Please follow these steps to contribute:

1. Fork the repository.
2. Create a branch (`git checkout -b feature/new-feature`).
3. Make your changes and commit them (`git commit -am 'Add new feature'`).
4. Push the branch (`git push origin feature/new-feature`).
5. Open a Pull Request.

## License

This project is licensed under the GNU GENERAL PUBLIC License. See the `LICENSE` file for details.

## Acknowledgements

Special thanks to [ethers-rs](https://github.com/gakonst/ethers-rs) and [alloy-rs](https://github.com/alloy-rs/alloy) for the inspiration and foundational work that helped shape this project.

- README created with the help of ChatGPT
