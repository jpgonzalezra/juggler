use alloy::providers::Provider;
use alloy::providers::RootProvider;
use alloy::pubsub::PubSubFrontend;
use alloy::transports::TransportError;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::RwLock;

#[derive(Clone, Debug)]
enum WsOrIpc {
    Ws(RootProvider<PubSubFrontend>),
    Ipc(RootProvider<PubSubFrontend>),
}

struct RpcBalancer {
    providers: Vec<Arc<WsOrIpc>>,
    current: AtomicUsize,
    last_node: RwLock<usize>,
}

impl RpcBalancer {
    pub fn new(providers: Vec<Arc<WsOrIpc>>) -> Self {
        RpcBalancer {
            providers,
            current: AtomicUsize::new(0),
            last_node: RwLock::new(0),
        }
    }

    async fn get_last_node(&self) -> usize {
        *self.last_node.read().await
    }

    async fn update_last_node(&self, index: usize) {
        let mut last_node = self.last_node.write().await;
        *last_node = index;
    }

    fn next_provider(&self) -> Arc<WsOrIpc> {
        let index = self.current.fetch_add(1, Ordering::SeqCst) % self.providers.len();
        self.providers[index].clone()
    }

    async fn execute<'a, F, Fut, U>(&'a self, mut func: F) -> Result<U, TransportError>
    where
        F: FnMut(Arc<WsOrIpc>) -> Fut,
        Fut: std::future::Future<Output = Result<U, TransportError>> + 'a,
    {
        for _ in 0..self.providers.len() {
            let provider = self.next_provider();
            match func(provider.clone()).await {
                Ok(result) => {
                    let index = self.current.load(Ordering::SeqCst) % self.providers.len();
                    self.update_last_node(index).await;
                    return Ok(result);
                }
                Err(_) => continue,
            }
        }
        Err(TransportError::local_usage_str("All providers failed"))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use alloy::{
        node_bindings::Anvil,
        providers::{ProviderBuilder, WsConnect},
        transports::RpcError,
    };
    use tokio::time::sleep;

    #[tokio::test]
    async fn test_rpc_balancer_with_node_failures() -> Result<(), RpcError<String>> {
        // Start three Anvil nodes with WebSockets
        let anvil1 = Anvil::new().port(8545 as u16).spawn();
        let anvil2 = Anvil::new().port(8546 as u16).spawn();
        let anvil3 = Anvil::new().port(8547 as u16).spawn();

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

        // Simulate all nodes failure by dropping anvil3
        drop(anvil3);

        sleep(Duration::from_secs(1)).await;

        let result = balancer
            .execute(|provider| async move { dummy_request(&provider).await })
            .await;

        // Check result after all nodes are down
        assert!(
            result.is_err(),
            "Request should fail after all nodes are down"
        );

        Ok(())
    }
}
