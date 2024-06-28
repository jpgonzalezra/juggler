use alloy::providers::RootProvider;
use alloy::pubsub::PubSubFrontend;
use alloy::pubsub::Subscription;
use alloy::transports::{RpcError, TransportError, TransportErrorKind};
use serde::de::DeserializeOwned;
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio_stream::StreamExt;
use tracing::{error, info, warn};

#[derive(Clone, Debug)]
enum WsOrIpc {
    Ws(RootProvider<PubSubFrontend>),
    Ipc(RootProvider<PubSubFrontend>),
}

struct RpcBalancer {
    providers: Vec<Arc<WsOrIpc>>,
    current: AtomicUsize,
}

impl RpcBalancer {
    pub fn new(providers: Vec<Arc<WsOrIpc>>) -> Self {
        RpcBalancer {
            providers,
            current: AtomicUsize::new(0),
        }
    }

    pub fn next_provider(&self) -> Arc<WsOrIpc> {
        let index = self.current.fetch_add(1, Ordering::SeqCst) % self.providers.len();
        self.providers[index].clone()
    }

    pub async fn execute<'a, F, Fut, U>(&'a self, mut func: F) -> Result<U, TransportError>
    where
        F: FnMut(Arc<WsOrIpc>) -> Fut,
        Fut: std::future::Future<Output = Result<U, TransportError>> + 'a,
    {
        for _ in 0..self.providers.len() {
            let provider = self.next_provider();
            match func(provider.clone()).await {
                Ok(result) => {
                    return Ok(result);
                }
                Err(_) => continue,
            }
        }
        Err(TransportError::local_usage_str("All providers failed"))
    }

    pub async fn subscribe<'a, P, R, F, Fut>(
        &'a self,
        params: P,
        mut handle_data: F,
    ) -> Result<(), TransportError>
    where
        P: for<'b> Fn(
                &'b RootProvider<PubSubFrontend>,
            ) -> Pin<
                Box<
                    dyn Future<Output = Result<Subscription<R>, RpcError<TransportErrorKind>>>
                        + Send
                        + 'b,
                >,
            > + 'a,
        R: 'static + DeserializeOwned + std::fmt::Debug,
        F: FnMut(R) -> Fut,
        Fut: std::future::Future<Output = ()> + 'static,
    {
        loop {
            match self.get_subscription(&params).await {
                Ok(subscription) => {
                    let mut stream = subscription.into_stream();
                    while let Some(data) = stream.next().await {
                        info!("New block: {:?}", data);
                        handle_data(data).await;
                    }
                }
                Err(e) => {
                    error!("Error obtaining subscription: {:?}", e);
                }
            }
        }
    }

    async fn get_subscription<'a, P, R>(
        &'a self,
        params: &P,
    ) -> Result<Subscription<R>, TransportError>
    where
        P: for<'b> Fn(
                &'b RootProvider<PubSubFrontend>,
            ) -> Pin<
                Box<
                    dyn Future<Output = Result<Subscription<R>, RpcError<TransportErrorKind>>>
                        + Send
                        + 'b,
                >,
            > + 'a,
        R: 'static + DeserializeOwned + std::fmt::Debug,
    {
        for _ in 0..self.providers.len() {
            let provider = self.next_provider();
            match self.try_subscribe(provider.clone(), params).await {
                Ok(subscription) => return Ok(subscription),
                Err(e) => {
                    warn!("Failed to subscribe with the current provider: {:?}. Switching to a different provider and retrying...", e);
                    continue;
                }
            }
        }
        Err(TransportError::local_usage_str("All providers failed"))
    }

    async fn try_subscribe<'a, P, R>(
        &'a self,
        provider: Arc<WsOrIpc>,
        params: &P,
    ) -> Result<Subscription<R>, TransportError>
    where
        P: for<'b> Fn(
                &'b RootProvider<PubSubFrontend>,
            ) -> Pin<
                Box<
                    dyn Future<Output = Result<Subscription<R>, RpcError<TransportErrorKind>>>
                        + Send
                        + 'b,
                >,
            > + 'a,
        R: 'static + DeserializeOwned + std::fmt::Debug,
    {
        match &*provider {
            WsOrIpc::Ws(ws_provider) => {
                let subscription = params(ws_provider).await?;
                Ok(subscription)
            }
            WsOrIpc::Ipc(ipc_provider) => {
                let subscription = params(ipc_provider).await?;
                Ok(subscription)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;
    use sysinfo::System;

    use super::*;
    use alloy::providers::Provider;
    use alloy::{
        node_bindings::Anvil,
        providers::{ProviderBuilder, WsConnect},
        transports::RpcError,
    };
    use tokio::time::sleep;

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
        tracing_subscriber::fmt::init();
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
            .execute(|provider| async move { dummy_request(&provider).await })
            .await;

        // Check initial result
        assert!(result.is_ok(), "Initial request should succeed");

        // Simulate node 1 failure by dropping anvil1
        drop(anvil1);

        // Wait for the node to be considered down
        sleep(Duration::from_secs(1)).await;

        let result = balancer
            .execute(|provider| async move { dummy_request(&provider).await })
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
            .execute(|provider| async move { dummy_request(&provider).await })
            .await;

        // Check result after node 2 failure
        assert!(
            result.is_ok(),
            "Request should succeed after node 2 failure"
        );

        sleep(Duration::from_secs(1)).await;

        let result = balancer
            .execute(|provider| async move { dummy_request(&provider).await })
            .await;

        // Check result after all nodes are down
        assert!(result.is_ok(), "Request should succeed again");

        Ok(())
    }

    #[tokio::test]
    async fn test_rpc_balancer_subscribe_blocks() -> Result<(), RpcError<String>> {
        init_tests();

        // Start three Anvil nodes with WebSockets
        let anvil1 = Anvil::new().port(8548 as u16).block_time(1).spawn();
        let anvil2 = Anvil::new().port(8549 as u16).block_time(1).spawn();
        let anvil3 = Anvil::new().port(8550 as u16).block_time(1).spawn();

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

        // Use the balancer to subscribe to blocks
        let subscribe_handle = tokio::spawn(async move {
            let result = balancer
                .subscribe(
                    |provider| Box::pin(provider.subscribe_blocks()),
                    |block| async move {
                        println!("New block: {:?}", block);
                    },
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
        _ = tokio::time::timeout(Duration::from_secs(10), subscribe_handle).await;

        Ok(())
    }
}
