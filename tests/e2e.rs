#[cfg(test)]
mod tests {
    use alloy::transports::TransportError;
    use alloy_resilient_rpc::provider::{RpcBalancer, WsOrIpc};
    use std::sync::{Arc, Once};
    use std::time::Duration;
    use sysinfo::System;

    use alloy::providers::Provider;
    use alloy::{
        node_bindings::Anvil,
        providers::{ProviderBuilder, WsConnect},
        transports::RpcError,
    };
    use tokio::time::sleep;

    static INIT: Once = Once::new();

    fn kill_anvil_processes() {
        let mut system = System::new_all();
        system.refresh_all();

        let ports_to_check = [8545, 8546, 8547];

        for (_, process) in system.processes() {
            for port in &ports_to_check {
                if process.name().contains("anvil")
                    && process
                        .cmd()
                        .iter()
                        .any(|arg| arg.contains(&port.to_string()))
                {
                    println!("Killing process: {} on port: {}", process.name(), port);
                    _ = process.kill()
                }
            }
        }
    }

    fn init_tests() {
        INIT.call_once(|| {
            tracing_subscriber::fmt::init();
        });
        kill_anvil_processes();
    }

    #[tokio::test]
    async fn test_rpc_balancer_with_node_failures() -> Result<(), RpcError<String>> {
        init_tests();

        // Start three Anvil nodes with WebSockets
        let anvil1 = Anvil::new().port(8545 as u16).block_time(1).spawn();
        let anvil2 = Anvil::new().port(8546 as u16).block_time(1).spawn();
        let anvil3 = Anvil::new().port(8547 as u16).block_time(1).spawn();

        // Create providers for each Anvil instance
        let ws_provider1 = WsConnect::new(anvil1.ws_endpoint().as_str());
        let ws_provider2 = WsConnect::new(anvil2.ws_endpoint().as_str());
        let ws_provider3 = WsConnect::new(anvil3.ws_endpoint().as_str());

        // Wrap them in Providers
        let provider1 = Arc::new(WsOrIpc::Ws(
            ProviderBuilder::new()
                .on_ws(ws_provider1)
                .await
                .map_err(|_| RpcError::local_usage_str("provider1 error"))?,
        ));
        let provider2 = Arc::new(WsOrIpc::Ws(
            ProviderBuilder::new()
                .on_ws(ws_provider2)
                .await
                .map_err(|_| RpcError::local_usage_str("provider2 error"))?,
        ));
        let provider3 = Arc::new(WsOrIpc::Ws(
            ProviderBuilder::new()
                .on_ws(ws_provider3)
                .await
                .map_err(|_| RpcError::local_usage_str("provider3 error"))?,
        ));

        // Initialize RpcBalancer with the providers
        let balancer = Arc::new(RpcBalancer::new(vec![
            provider1.clone(),
            provider2.clone(),
            provider3.clone(),
        ]));

        // Dummy function to execute
        async fn dummy_request(provider: &WsOrIpc) -> Result<u64, TransportError> {
            match provider {
                WsOrIpc::Ws(ws_provider) => {
                    // Simulate a request to the WebSocket provider
                    ws_provider.get_block_number().await.map_err(|e| e.into())
                }
                WsOrIpc::Ipc(_) => {
                    // IPC not used in this test
                    Err(TransportError::local_usage_str(
                        "IPC not supported in this test",
                    ))
                }
            }
        }

        // Use the balancer to execute the dummy request
        let result = balancer
            .execute(Box::new(|provider: Arc<WsOrIpc>| {
                Box::pin(async move { dummy_request(&provider).await })
            }))
            .await;

        // Check initial result
        assert!(result.is_ok(), "Initial request should succeed");

        // Simulate node 1 failure by dropping anvil1
        drop(anvil1);

        // Wait for the node to be considered down
        sleep(Duration::from_secs(1)).await;

        let result = balancer
            .execute(Box::new(|provider: Arc<WsOrIpc>| {
                Box::pin(async move { dummy_request(&provider).await })
            }))
            .await;

        // Check result after node 1 failure
        assert!(
            result.is_ok(),
            "Request should succeed after node 1 failure"
        );

        // Simulate node 2 failure by dropping anvil2
        drop(anvil2);

        // Wait for the node to be considered down
        sleep(Duration::from_secs(1)).await;

        let result = balancer
            .execute(Box::new(|provider: Arc<WsOrIpc>| {
                Box::pin(async move { dummy_request(&provider).await })
            }))
            .await;

        // Check result after node 2 failure
        assert!(
            result.is_ok(),
            "Request should succeed after node 2 failure"
        );

        sleep(Duration::from_secs(1)).await;

        let result = balancer
            .execute(Box::new(|provider: Arc<WsOrIpc>| {
                Box::pin(async move { dummy_request(&provider).await })
            }))
            .await;

        // Check result after all nodes are down
        assert!(result.is_ok(), "Request should succeed again");

        Ok(())
    }

    #[tokio::test]
    async fn test_rpc_balancer_subscribe_blocks() -> Result<(), RpcError<String>> {
        init_tests();

        // Start three Anvil nodes with WebSockets
        let anvil1 = Anvil::new().port(8545 as u16).block_time(1).spawn();
        let anvil2 = Anvil::new().port(8546 as u16).block_time(1).spawn();
        let anvil3 = Anvil::new().port(8547 as u16).block_time(1).spawn();

        // Create providers for each Anvil instance
        let ws_provider1 = WsConnect::new(anvil1.ws_endpoint().as_str());
        let ws_provider2 = WsConnect::new(anvil2.ws_endpoint().as_str());
        let ws_provider3 = WsConnect::new(anvil3.ws_endpoint().as_str());

        // Wrap them in Providers
        let provider1 = Arc::new(WsOrIpc::Ws(
            ProviderBuilder::new()
                .on_ws(ws_provider1)
                .await
                .map_err(|_| RpcError::local_usage_str("provider1 error"))?,
        ));
        let provider2 = Arc::new(WsOrIpc::Ws(
            ProviderBuilder::new()
                .on_ws(ws_provider2)
                .await
                .map_err(|_| RpcError::local_usage_str("provider2 error"))?,
        ));
        let provider3 = Arc::new(WsOrIpc::Ws(
            ProviderBuilder::new()
                .on_ws(ws_provider3)
                .await
                .map_err(|_| RpcError::local_usage_str("provider3 error"))?,
        ));

        // Initialize RpcBalancer with the providers
        let balancer = Arc::new(RpcBalancer::new(vec![
            provider1.clone(),
            provider2.clone(),
            provider3.clone(),
        ]));

        let handle_data = Box::new(|block| {
            Box::pin(async move {
                println!("New block: {:?}", block);
            })
        });

        // Use the balancer to subscribe to blocks
        let subscribe_handle = tokio::spawn(async move {
            let result = balancer
                .subscribe(
                    |provider| Box::pin(provider.subscribe_blocks()),
                    handle_data,
                )
                .await;

            assert!(
                result.is_err(),
                "Subscription should eventually fail if all nodes are down"
            );
        });

        // Simulate node failures
        sleep(Duration::from_secs(2)).await;
        drop(anvil1);

        sleep(Duration::from_secs(2)).await;
        drop(anvil2);

        // Wait for the subscription task to finish
        _ = tokio::time::timeout(Duration::from_secs(5), subscribe_handle).await;

        Ok(())
    }

    // #[tokio::test]
    async fn test_rpc_balancer_with_timeouts() -> Result<(), RpcError<String>> {
        init_tests();

        // Start two Anvil nodes with WebSockets
        let anvil1 = Anvil::new().port(8545 as u16).block_time(1).spawn();
        let anvil2 = Anvil::new().port(8546 as u16).block_time(1).spawn();

        // Create providers for each Anvil instance
        let ws_provider1 = WsConnect::new(anvil1.ws_endpoint().as_str());
        let ws_provider2 = WsConnect::new(anvil2.ws_endpoint().as_str());

        // Wrap them in Providers
        let provider1 = Arc::new(WsOrIpc::Ws(
            ProviderBuilder::new()
                .on_ws(ws_provider1)
                .await
                .map_err(|_| RpcError::local_usage_str("provider1 error"))?,
        ));
        let provider2 = Arc::new(WsOrIpc::Ws(
            ProviderBuilder::new()
                .on_ws(ws_provider2)
                .await
                .map_err(|_| RpcError::local_usage_str("provider2 error"))?,
        ));

        // Initialize RpcBalancer with the providers
        let balancer = Arc::new(RpcBalancer::new(vec![provider1.clone(), provider2.clone()]));

        // Clone the balancer for the subscription handle

        let handle_data = Box::new(|block| {
            Box::pin({
                async move {
                    println!("New block: {:?}", block);
                }
            })
        });

        // Use the balancer to subscribe to blocks
        let subscribe_handle = tokio::spawn(async move {
            let result = balancer
                .subscribe(
                    |provider| Box::pin(provider.subscribe_blocks()),
                    handle_data,
                )
                .await;

            assert!(
                result.is_err(),
                "Subscription should eventually fail if all nodes are down"
            );
        });

        // Simulate node timeouts
        sleep(Duration::from_secs(2)).await;

        // Temporarily stop anvil1
        drop(anvil1);

        // Wait a bit and then restart anvil1
        sleep(Duration::from_secs(5)).await;

        // Temporarily stop anvil2
        drop(anvil2);

        kill_anvil_processes();

        let _anvil1 = Anvil::new().port(8545 as u16).block_time(1).spawn();

        // Wait a bit and then restart anvil2
        sleep(Duration::from_secs(5)).await;
        let _anvil2 = Anvil::new().port(8546 as u16).block_time(1).spawn();

        // Wait for the subscription task to finish
        _ = tokio::time::timeout(Duration::from_secs(10), subscribe_handle).await;

        Ok(())
    }
}
