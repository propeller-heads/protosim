use chrono::NaiveDateTime;
use futures::StreamExt;
use hyper::{client::HttpConnector, Body, Client, Request, Uri};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, string::ToString};
use thiserror::Error;
use tracing::{error, info};

use super::tycho_models::{Block, BlockStateChanges, Chain};
use async_trait::async_trait;
use revm::primitives::{B160, B256, U256 as rU256};
use tokio::sync::mpsc::{self, Receiver};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};

#[derive(Error, Debug)]
pub enum TychoClientError {
    #[error("Failed to parse URI: {0}. Error: {1}")]
    UriParsing(String, String),
    #[error("Failed to format request: {0}")]
    FormatRequest(String),
    #[error("Unexpected HTTP client error: {0}")]
    HttpClient(String),
    #[error("Failed to parse response: {0}")]
    ParseResponse(String),
}

#[derive(Serialize, Debug, Default)]
pub struct StateRequestBody {
    contract_ids: Option<Vec<B160>>,
    version: Option<Version>,
}
impl StateRequestBody {
    pub fn from_block(block: Block) -> Self {
        Self {
            contract_ids: None,
            version: Some(Version {
                timestamp: None,
                block: Some(RequestBlock {
                    hash: block.hash,
                    number: block.number,
                    chain: block.chain,
                }),
            }),
        }
    }
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct StateRequestParameters {
    #[serde(default = "Chain::default")]
    chain: Chain,
    tvl_gt: Option<u64>,
    intertia_min_gt: Option<u64>,
}

impl StateRequestParameters {
    pub fn to_query_string(&self) -> String {
        let mut parts = vec![];

        parts.push(format!("chain={}", self.chain));

        if let Some(tvl_gt) = self.tvl_gt {
            parts.push(format!("tvl_gt={}", tvl_gt));
        }

        if let Some(inertia) = self.intertia_min_gt {
            parts.push(format!("intertia_min_gt={}", inertia));
        }

        parts.join("&")
    }
}

#[derive(Serialize, Debug)]
pub struct Version {
    timestamp: Option<NaiveDateTime>,
    block: Option<RequestBlock>,
}

#[derive(Serialize, Debug)]
struct RequestBlock {
    hash: B256,
    number: u64,
    chain: Chain,
}

#[derive(Deserialize, Clone, Debug)]
pub struct ResponseAccount {
    pub address: B160,
    pub slots: HashMap<rU256, rU256>,
    pub balance: rU256,
    pub code: Vec<u8>,
    pub code_hash: B256,
}

pub struct TychoClient {
    http_client: Client<HttpConnector>,
    base_uri: Uri,
}
impl TychoClient {
    pub fn new(base_uri: &str) -> Result<Self, TychoClientError> {
        let base_uri = base_uri
            .parse::<Uri>()
            .map_err(|e| TychoClientError::UriParsing(base_uri.to_string(), e.to_string()))?;

        Ok(Self { http_client: Client::new(), base_uri })
    }
}

#[async_trait]
pub trait TychoVMStateClient {
    async fn get_state(
        &self,
        filters: &StateRequestParameters,
        request: &StateRequestBody,
    ) -> Result<Vec<ResponseAccount>, TychoClientError>;

    async fn realtime_messages(&self) -> Receiver<BlockStateChanges>;
}

#[async_trait]
impl TychoVMStateClient for TychoClient {
    async fn get_state(
        &self,
        filters: &StateRequestParameters,
        request: &StateRequestBody,
    ) -> Result<Vec<ResponseAccount>, TychoClientError> {
        let url = format!(
            "http://{}/contract_state?{}",
            self.base_uri
                .to_string()
                .trim_end_matches('/'),
            filters.to_query_string()
        );

        let body = serde_json::to_string(&request)
            .map_err(|e| TychoClientError::FormatRequest(e.to_string()))?;

        let req = Request::get(url)
            .body(Body::from(body))
            .map_err(|e| TychoClientError::FormatRequest(e.to_string()))?;

        let response = self
            .http_client
            .request(req)
            .await
            .map_err(|e| TychoClientError::HttpClient(e.to_string()))?;

        let body = hyper::body::to_bytes(response.into_body())
            .await
            .map_err(|e| TychoClientError::ParseResponse(e.to_string()))?;
        let accounts: Vec<ResponseAccount> = serde_json::from_slice(&body)
            .map_err(|e| TychoClientError::ParseResponse(e.to_string()))?;

        Ok(accounts)
    }

    async fn realtime_messages(&self) -> Receiver<BlockStateChanges> {
        // Create a channel to send and receive messages.
        let (tx, rx) = mpsc::channel(30); //TODO: Set this properly.

        // Spawn a task to connect to the WebSocket server and listen for realtime messages.
        let ws_url = format!("ws://{}", self.base_uri);
        tokio::spawn(async move {
            let ws_stream = match connect_async(&ws_url).await {
                Ok((ws, _)) => ws,
                Err(e) => {
                    error!("Failed to connect to WebSocket: {:?}", e);
                    return
                }
            };

            // Use the stream directly to listen for messages.
            let mut incoming_messages = ws_stream.boxed();

            while let Some(msg) = incoming_messages.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        match serde_json::from_str::<BlockStateChanges>(&text) {
                            Ok(update) => match tx.send(update).await {
                                Ok(_) => {}
                                Err(e) => {
                                    //TODO: This might happen if the receiver is dropped (meaning
                                    // the update_loop received the stop signal).
                                    // We should catch this error and end this loop.
                                    error!(error = %e, "Failed to send message to the channel")
                                }
                            },
                            Err(e) => {
                                // Handle the error, perhaps log it.
                                error!(error = %e, "Failed to deserialize message")
                            }
                        }
                    }
                    Ok(Message::Close(_)) => {
                        drop(tx);
                        return
                    }
                    Ok(unknown_msg) => {
                        info!("Received an unknown message type: {:?}", unknown_msg);
                    }
                    Err(e) => {
                        error!("Failed to get a websocket message: {}", e);
                    }
                }
            }
        });

        rx
    }
}

#[cfg(test)]
mod tests {
    use crate::evm_simulation::tycho_models::{AccountUpdateWithTx, ChangeType, Transaction};

    use super::*;

    use mockito::Server;
    use std::str::FromStr;

    use futures::SinkExt;
    use warp::{ws::WebSocket, Filter};

    #[tokio::test]
    async fn test_realtime_messages() {
        // Mock WebSocket server using warp
        async fn handle_connection(ws: WebSocket) {
            let (mut tx, _) = ws.split();
            let test_msg = warp::ws::Message::text(
                r#"
            {
                "extractor": "ambient",
                "chain": "ethereum",
                "block": {
                    "number": 123,
                    "hash": "0x0000000000000000000000000000000000000000000000000000000000000000",
                    "parent_hash": "0x0000000000000000000000000000000000000000000000000000000000000000",
                    "chain": "ethereum",
                    "ts": "2023-09-14T00:00:00"
                },
                "tx_updates": [
                    {
                        "update": {
                            "address": "0x7a250d5630b4cf539739df2c5dacb4c659f2488d",
                            "chain": "ethereum",
                            "slots": {},
                            "balance": "0x00000000000000000000000000000000000000000000000000000000000001f4",
                            "code": [],
                            "change": "Update"
                        },
                        "tx": {
                            "hash": "0x0000000000000000000000000000000000000000000000000000000000000000",
                            "block_hash": "0x0000000000000000000000000000000000000000000000000000000000000000",
                            "from": "0x000000000000000000000000000000000000007b",
                            "to": "0xb2e16d0168e52d35cacd2c6185b44281ec28c9dc",
                            "index": 1
                        }
                    }
                ],
                "new_pools": {}
            }
            "#,
            );
            let _ = tx.send(test_msg).await;
        }

        let ws_route = warp::ws().map(|ws: warp::ws::Ws| ws.on_upgrade(handle_connection));
        let (addr, server) = warp::serve(ws_route).bind_ephemeral(([127, 0, 0, 1], 0));
        tokio::task::spawn(server);

        // Now, you can create a client and connect to the mocked WebSocket server
        let client = TychoClient::new(&format!("{}", addr)).unwrap();

        // You can listen to the realtime_messages and expect the messages that you send from
        // handle_connection
        let mut rx = client.realtime_messages().await;
        let received_msg = rx
            .recv()
            .await
            .expect("receive message");

        let expected_blk = Block {
            number: 123,
            hash: B256::from_str(
                "0x0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap(),
            parent_hash: B256::from_str(
                "0x0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap(),
            chain: Chain::Ethereum,
            ts: NaiveDateTime::from_str("2023-09-14T00:00:00").unwrap(),
        };

        let transaction = Transaction {
            hash: B256::from_str(
                "0x0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap(),
            block_hash: B256::from_str(
                "0x0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap(),
            from: B160::from_str("0x000000000000000000000000000000000000007b").unwrap(),
            to: Some(B160::from_str("0xb2e16d0168e52d35cacd2c6185b44281ec28c9dc").unwrap()),
            index: 1,
        };
        let account_update = AccountUpdateWithTx::new(
            B160::from_str("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D").unwrap(),
            Chain::Ethereum,
            HashMap::new(),
            Some(rU256::from(500)),
            Some(Vec::<u8>::new()),
            ChangeType::Update,
            transaction,
        );
        let expected = BlockStateChanges {
            extractor: "ambient".to_string(),
            chain: Chain::Ethereum,
            block: expected_blk,
            tx_updates: vec![account_update],
            new_pools: HashMap::new(),
        };

        assert_eq!(received_msg, expected);
    }

    #[tokio::test]
    async fn test_simple_route_mock_async() {
        let mut server = Server::new_async().await;
        let server_resp = r#"
        [{
            "address": "0xb4e16d0168e52d35cacd2c6185b44281ec28c9dc",
            "slots": {},
            "balance": "0x1f4",
            "code": [],
            "code_hash": "0x5c06b7c5b3d910fd33bc2229846f9ddaf91d584d9b196e16636901ac3a77077e"
        }]
        "#;
        let mocked_server = server
            .mock("GET", "/contract_state?chain=ethereum")
            .expect(1)
            .with_body(server_resp)
            .create_async()
            .await;

        let client = TychoClient::new(
            server
                .url()
                .replace("http://", "")
                .as_str(),
        )
        .expect("create client");

        let response = client
            .get_state(&Default::default(), &Default::default())
            .await
            .expect("get state");

        mocked_server.assert();
        assert_eq!(response.len(), 1);
        assert_eq!(response[0].slots, HashMap::new());
        assert_eq!(response[0].balance, rU256::from(500));
        assert_eq!(response[0].code, Vec::<u8>::new());
        assert_eq!(
            response[0].code_hash,
            B256::from_str("0x5c06b7c5b3d910fd33bc2229846f9ddaf91d584d9b196e16636901ac3a77077e")
                .unwrap()
        );
    }
}
