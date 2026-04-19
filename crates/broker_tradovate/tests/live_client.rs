use chrono::{Duration, Utc};
use futures_util::{SinkExt, StreamExt};
use mockito::{Matcher, Server};
use rust_decimal::Decimal;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::{accept_async, tungstenite::Message};
use tv_bot_broker_tradovate::{
    TradovateAccountApi, TradovateAccountListRequest, TradovateAuthApi, TradovateAuthRequest,
    TradovateBracketOrder, TradovateCredentials, TradovateExecutionApi, TradovateExecutionContext,
    TradovateLiquidatePositionRequest, TradovateLiveClient, TradovateLiveClientConfig,
    TradovateOrderPlacement, TradovateOrderType, TradovateOsoOrderPlacement,
    TradovatePlaceOrderRequest, TradovatePlaceOsoRequest, TradovateRenewAccessTokenRequest,
    TradovateSyncApi, TradovateSyncConnectRequest, TradovateSyncEvent, TradovateTimeInForce,
    TradovateUserSyncRequest,
};
use tv_bot_core_types::{BrokerEnvironment, BrokerOrderStatus, TradeSide};

fn sample_credentials() -> TradovateCredentials {
    TradovateCredentials {
        username: "bot-user".to_owned(),
        password: SecretString::new("password".to_owned().into()),
        cid: "123".to_owned(),
        sec: SecretString::new("sec-456".to_owned().into()),
        app_id: "tv-bot-core".to_owned(),
        app_version: "0.1.0".to_owned(),
        device_id: Some("desktop".to_owned()),
    }
}

#[tokio::test]
async fn live_client_requests_access_token_and_lists_accounts() {
    let mut server = Server::new_async().await;
    let expires_at = (Utc::now() + Duration::minutes(90)).to_rfc3339();

    let auth_mock = server
        .mock("POST", "/v1/auth/accesstokenrequest")
        .match_header(
            "content-type",
            Matcher::Regex("application/json".to_owned()),
        )
        .match_body(Matcher::Regex("\"name\":\"bot-user\"".to_owned()))
        .match_body(Matcher::Regex("\"cid\":123".to_owned()))
        .with_status(200)
        .with_body(
            json!({
                "accessToken": "access-token",
                "expirationTime": expires_at,
                "userId": 77,
                "personId": 88,
                "mdAccessToken": "md-token"
            })
            .to_string(),
        )
        .create_async()
        .await;

    let accounts_mock = server
        .mock("GET", "/v1/account/list")
        .match_header("authorization", "Bearer access-token")
        .with_status(200)
        .with_body(
            json!([
                { "id": 101, "name": "paper-primary", "active": true },
                { "id": 202, "name": "paper-secondary", "nickname": "alt", "active": false }
            ])
            .to_string(),
        )
        .create_async()
        .await;

    let client = TradovateLiveClient::new(TradovateLiveClientConfig::default());
    let base_url = format!("{}/v1", server.url());

    let token = client
        .request_access_token(TradovateAuthRequest {
            http_base_url: base_url.clone(),
            environment: BrokerEnvironment::Demo,
            credentials: sample_credentials(),
        })
        .await
        .expect("access token request should succeed");

    let accounts = client
        .list_accounts(TradovateAccountListRequest {
            http_base_url: base_url,
            environment: BrokerEnvironment::Demo,
            access_token: token.clone(),
        })
        .await
        .expect("account list should succeed");

    auth_mock.assert_async().await;
    accounts_mock.assert_async().await;

    assert_eq!(token.access_token.expose_secret(), "access-token");
    assert_eq!(token.user_id, Some(77));
    assert_eq!(token.market_data_access.as_deref(), Some("md-token"));
    assert_eq!(accounts.len(), 2);
    assert_eq!(accounts[0].account_name, "paper-primary");
    assert!(!accounts[1].active);
}

#[tokio::test]
async fn live_client_renews_access_token_over_bearer_auth() {
    let mut server = Server::new_async().await;
    let expires_at = (Utc::now() + Duration::minutes(90)).to_rfc3339();

    let renew_mock = server
        .mock("GET", "/v1/auth/renewaccesstoken")
        .match_header("authorization", "Bearer old-token")
        .with_status(200)
        .with_body(
            json!({
                "accessToken": "new-token",
                "expirationTime": expires_at,
                "userId": 77
            })
            .to_string(),
        )
        .create_async()
        .await;

    let client = TradovateLiveClient::new(TradovateLiveClientConfig::default());
    let renewed = client
        .renew_access_token(TradovateRenewAccessTokenRequest {
            http_base_url: format!("{}/v1", server.url()),
            environment: BrokerEnvironment::Demo,
            current_token: tv_bot_broker_tradovate::TradovateAccessToken {
                access_token: SecretString::new("old-token".to_owned().into()),
                expiration_time: Utc::now() + Duration::minutes(1),
                issued_at: Utc::now(),
                user_id: Some(77),
                person_id: None,
                market_data_access: None,
            },
        })
        .await
        .expect("renew should succeed");

    renew_mock.assert_async().await;
    assert_eq!(renewed.access_token.expose_secret(), "new-token");
}

#[tokio::test]
async fn live_client_authorizes_syncs_and_processes_props_and_heartbeats() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let address = listener.local_addr().expect("listener addr should exist");

    let server_task = tokio::spawn(async move {
        let (stream, _) = listener
            .accept()
            .await
            .expect("socket accept should succeed");
        let mut websocket = accept_async(stream)
            .await
            .expect("websocket handshake should succeed");

        websocket
            .send(Message::Text("o".to_owned().into()))
            .await
            .expect("open frame should send");

        let authorize = websocket
            .next()
            .await
            .expect("authorize frame should arrive")
            .expect("authorize frame should be valid")
            .into_text()
            .expect("authorize frame should be text");
        assert_eq!(authorize.as_str(), "authorize\n0\n\naccess-token");

        websocket
            .send(Message::Text(r#"a[{"i":0,"s":200}]"#.to_owned().into()))
            .await
            .expect("auth ack should send");

        let sync_request = websocket
            .next()
            .await
            .expect("sync frame should arrive")
            .expect("sync frame should be valid")
            .into_text()
            .expect("sync frame should be text");
        assert!(sync_request.starts_with("user/syncrequest\n1\n\n"));

        let body = sync_request
            .splitn(4, '\n')
            .nth(3)
            .expect("sync body should exist");
        let body_json: Value = serde_json::from_str(body).expect("sync body should be JSON");
        assert_eq!(body_json["splitResponses"], Value::Bool(false));
        assert_eq!(body_json["accounts"], json!([101]));
        assert!(body_json["entityTypes"]
            .as_array()
            .expect("entityTypes should be an array")
            .iter()
            .any(|value| value.as_str() == Some("position")));

        websocket
            .send(Message::Text(
                json!([{
                    "i": 1,
                    "s": 200,
                    "d": {
                        "positions": [{
                            "symbol": "GCM6",
                            "netPos": 1,
                            "averagePrice": "2385.1",
                            "timestamp": "2026-04-10T13:30:00Z"
                        }],
                        "orders": [{
                            "id": 9001,
                            "symbol": "GCM6",
                            "ordStatus": "Working",
                            "fillQty": 0,
                            "price": "2386.0",
                            "timestamp": "2026-04-10T13:30:00Z"
                        }],
                        "fills": [{
                            "id": 7001,
                            "orderId": 9001,
                            "symbol": "GCM6",
                            "action": "Buy",
                            "qty": 1,
                            "price": "2385.2",
                            "commission": "2.50",
                            "fee": "1.25",
                            "timestamp": "2026-04-10T13:30:00Z"
                        }],
                        "accounts": [{
                            "id": 101,
                            "name": "paper-primary",
                            "netLiq": "100000.0",
                            "timestamp": "2026-04-10T13:30:00Z"
                        }],
                        "cashBalances": [{
                            "accountId": 101,
                            "cashBalance": "50000.0",
                            "realizedPnL": "125.0",
                            "timestamp": "2026-04-10T13:30:00Z"
                        }],
                        "accountRiskStatuses": [{
                            "accountId": 101,
                            "availableFunds": "75000.0",
                            "excessLiquidity": "74500.0",
                            "status": "healthy",
                            "timestamp": "2026-04-10T13:30:00Z"
                        }]
                    }
                }])
                .to_string()
                .replacen('[', "a[", 1)
                .into(),
            ))
            .await
            .expect("sync snapshot should send");

        websocket
            .send(Message::Text(
                json!({
                    "e": "props",
                    "d": {
                        "entityType": "order",
                        "eventType": "Created",
                        "entity": {
                            "id": 9002,
                            "symbol": "GCM6",
                            "ordStatus": "Pending",
                            "fillQty": 0,
                            "price": "2387.0",
                            "timestamp": "2026-04-10T13:31:00Z"
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("props event should send");

        websocket
            .send(Message::Text(
                json!({
                    "e": "props",
                    "d": {
                        "entityType": "fill",
                        "eventType": "Created",
                        "entity": {
                            "id": 7002,
                            "orderId": 9002,
                            "symbol": "GCM6",
                            "action": "Buy",
                            "qty": 1,
                            "price": "2386.5",
                            "commission": "2.75",
                            "fee": "1.30",
                            "timestamp": "2026-04-10T13:31:05Z"
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("fill props event should send");

        websocket
            .send(Message::Text(
                json!({
                    "e": "props",
                    "d": {
                        "entityType": "cashBalance",
                        "eventType": "Updated",
                        "entity": {
                            "accountId": 101,
                            "cashBalance": "50500.0",
                            "unrealizedPnL": "140.0",
                            "timestamp": "2026-04-10T13:31:10Z"
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("cash balance props event should send");

        websocket
            .send(Message::Text("o".to_owned().into()))
            .await
            .expect("heartbeat frame should send");

        let heartbeat_ack = websocket
            .next()
            .await
            .expect("heartbeat ack should arrive")
            .expect("heartbeat ack should be valid")
            .into_text()
            .expect("heartbeat ack should be text");
        assert_eq!(heartbeat_ack.as_str(), "[]");
    });

    let client = TradovateLiveClient::new(TradovateLiveClientConfig::default());
    let token = tv_bot_broker_tradovate::TradovateAccessToken {
        access_token: SecretString::new("access-token".to_owned().into()),
        expiration_time: Utc::now() + Duration::minutes(30),
        issued_at: Utc::now(),
        user_id: Some(77),
        person_id: None,
        market_data_access: None,
    };

    client
        .connect(TradovateSyncConnectRequest {
            websocket_url: format!("ws://{address}"),
            environment: BrokerEnvironment::Demo,
            access_token: token.clone(),
        })
        .await
        .expect("websocket connect should succeed");

    let initial_snapshot = client
        .request_user_sync(TradovateUserSyncRequest {
            account_id: 101,
            access_token: token,
        })
        .await
        .expect("user sync should succeed");

    assert_eq!(initial_snapshot.positions.len(), 1);
    assert_eq!(initial_snapshot.working_orders.len(), 1);
    assert_eq!(initial_snapshot.fills.len(), 1);
    assert_eq!(initial_snapshot.positions[0].symbol, "GCM6");
    assert_eq!(
        initial_snapshot
            .account_snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.account_name.as_deref()),
        Some("paper-primary")
    );
    assert_eq!(
        initial_snapshot
            .account_snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.cash_balance),
        Some(Decimal::new(50_000_0, 1))
    );

    let order_event = client
        .next_event()
        .await
        .expect("props read should succeed")
        .expect("props event should exist");

    match order_event {
        TradovateSyncEvent::SyncSnapshot { snapshot } => {
            assert_eq!(snapshot.working_orders.len(), 2);
            assert!(snapshot
                .working_orders
                .iter()
                .any(|order| order.broker_order_id == "9002"
                    && order.status == BrokerOrderStatus::Pending));
        }
        other => panic!("unexpected sync event: {other:?}"),
    }

    let fill_event = client
        .next_event()
        .await
        .expect("fill props read should succeed")
        .expect("fill props event should exist");

    match fill_event {
        TradovateSyncEvent::SyncSnapshot { snapshot } => {
            assert_eq!(snapshot.fills.len(), 2);
            assert!(snapshot.fills.iter().any(|fill| fill.fill_id == "7002"));
        }
        other => panic!("unexpected fill sync event: {other:?}"),
    }

    let account_event = client
        .next_event()
        .await
        .expect("account props read should succeed")
        .expect("account props event should exist");

    match account_event {
        TradovateSyncEvent::SyncSnapshot { snapshot } => {
            assert_eq!(
                snapshot
                    .account_snapshot
                    .as_ref()
                    .and_then(|account| account.cash_balance),
                Some(Decimal::new(50_500_0, 1))
            );
            assert_eq!(
                snapshot
                    .account_snapshot
                    .as_ref()
                    .and_then(|account| account.unrealized_pnl),
                Some(Decimal::new(140_0, 1))
            );
        }
        other => panic!("unexpected account sync event: {other:?}"),
    }

    let heartbeat_event = client
        .next_event()
        .await
        .expect("heartbeat read should succeed")
        .expect("heartbeat event should exist");

    match heartbeat_event {
        TradovateSyncEvent::Heartbeat { .. } => {}
        other => panic!("unexpected heartbeat event: {other:?}"),
    }

    client
        .disconnect()
        .await
        .expect("disconnect should succeed");
    server_task.await.expect("server task should complete");
}

#[tokio::test]
async fn live_client_retries_user_sync_when_tradovate_returns_continuation_ticket() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let address = listener.local_addr().expect("listener addr should exist");

    let server_task = tokio::spawn(async move {
        let (stream, _) = listener
            .accept()
            .await
            .expect("socket accept should succeed");
        let mut websocket = accept_async(stream)
            .await
            .expect("websocket handshake should succeed");

        websocket
            .send(Message::Text("o".to_owned().into()))
            .await
            .expect("open frame should send");

        let authorize = websocket
            .next()
            .await
            .expect("authorize frame should arrive")
            .expect("authorize frame should be valid")
            .into_text()
            .expect("authorize frame should be text");
        assert_eq!(authorize.as_str(), "authorize\n0\n\naccess-token");

        websocket
            .send(Message::Text(r#"a[{"i":0,"s":200}]"#.to_owned().into()))
            .await
            .expect("auth ack should send");

        let first_sync_request = websocket
            .next()
            .await
            .expect("first sync frame should arrive")
            .expect("first sync frame should be valid")
            .into_text()
            .expect("first sync frame should be text");
        assert!(first_sync_request.starts_with("user/syncrequest\n1\n\n"));

        let first_body = first_sync_request
            .splitn(4, '\n')
            .nth(3)
            .expect("first sync body should exist");
        let first_body_json: Value =
            serde_json::from_str(first_body).expect("first sync body should be JSON");
        assert_eq!(first_body_json["accounts"], json!([101]));
        assert_eq!(first_body_json.get("p-ticket"), None);

        websocket
            .send(Message::Text(
                json!([{
                    "i": 1,
                    "s": 200,
                    "d": {
                        "p-ticket": "continue-sync",
                        "p-time": 0
                    }
                }])
                .to_string()
                .replacen('[', "a[", 1)
                .into(),
            ))
            .await
            .expect("continuation response should send");

        let second_sync_request = websocket
            .next()
            .await
            .expect("second sync frame should arrive")
            .expect("second sync frame should be valid")
            .into_text()
            .expect("second sync frame should be text");
        assert!(second_sync_request.starts_with("user/syncrequest\n2\n\n"));

        let second_body = second_sync_request
            .splitn(4, '\n')
            .nth(3)
            .expect("second sync body should exist");
        let second_body_json: Value =
            serde_json::from_str(second_body).expect("second sync body should be JSON");
        assert_eq!(second_body_json["accounts"], json!([101]));
        assert_eq!(
            second_body_json["p-ticket"],
            Value::String("continue-sync".to_owned())
        );

        websocket
            .send(Message::Text(
                json!([{
                    "i": 2,
                    "s": 200,
                    "d": {
                        "positions": [{
                            "symbol": "GCM6",
                            "netPos": 2,
                            "averagePrice": "2385.1",
                            "timestamp": "2026-04-10T13:30:00Z"
                        }],
                        "orders": [],
                        "fills": [],
                        "accounts": [{
                            "id": 101,
                            "name": "paper-primary",
                            "netLiq": "100000.0",
                            "timestamp": "2026-04-10T13:30:00Z"
                        }]
                    }
                }])
                .to_string()
                .replacen('[', "a[", 1)
                .into(),
            ))
            .await
            .expect("final sync snapshot should send");

        let close_frame = websocket
            .next()
            .await
            .expect("close frame should arrive")
            .expect("close frame should be valid");
        assert!(matches!(close_frame, Message::Close(_)));
    });

    let client = TradovateLiveClient::new(TradovateLiveClientConfig::default());
    let token = tv_bot_broker_tradovate::TradovateAccessToken {
        access_token: SecretString::new("access-token".to_owned().into()),
        expiration_time: Utc::now() + Duration::minutes(30),
        issued_at: Utc::now(),
        user_id: Some(77),
        person_id: None,
        market_data_access: None,
    };

    client
        .connect(TradovateSyncConnectRequest {
            websocket_url: format!("ws://{address}"),
            environment: BrokerEnvironment::Demo,
            access_token: token.clone(),
        })
        .await
        .expect("websocket connect should succeed");

    let snapshot = client
        .request_user_sync(TradovateUserSyncRequest {
            account_id: 101,
            access_token: token,
        })
        .await
        .expect("user sync should succeed");

    assert_eq!(snapshot.positions.len(), 1);
    assert_eq!(snapshot.positions[0].quantity, 2);

    client
        .disconnect()
        .await
        .expect("disconnect should succeed");
    server_task.await.expect("server task should complete");
}

#[tokio::test]
async fn live_client_submits_place_order_place_oso_and_liquidation_requests() {
    let mut server = Server::new_async().await;
    let client = TradovateLiveClient::new(TradovateLiveClientConfig::default());
    let context = TradovateExecutionContext {
        http_base_url: format!("{}/v1", server.url()),
        access_token: tv_bot_broker_tradovate::TradovateAccessToken {
            access_token: SecretString::new("execution-token".to_owned().into()),
            expiration_time: Utc::now() + Duration::minutes(30),
            issued_at: Utc::now(),
            user_id: Some(77),
            person_id: None,
            market_data_access: None,
        },
        account_id: 101,
        account_spec: "paper-primary".to_owned(),
    };

    let place_order_mock = server
        .mock("POST", "/v1/order/placeorder")
        .match_header("authorization", "Bearer execution-token")
        .match_body(Matcher::Regex("\"accountId\":101".to_owned()))
        .match_body(Matcher::Regex(
            "\"accountSpec\":\"paper-primary\"".to_owned(),
        ))
        .match_body(Matcher::Regex("\"symbol\":\"GCM6\"".to_owned()))
        .match_body(Matcher::Regex("\"orderType\":\"Limit\"".to_owned()))
        .match_body(Matcher::Regex("\"price\":2385.1".to_owned()))
        .with_status(200)
        .with_body(json!({"failureReason": "Success", "orderId": 5001}).to_string())
        .create_async()
        .await;

    let place_oso_mock = server
        .mock("POST", "/v1/order/placeoso")
        .match_header("authorization", "Bearer execution-token")
        .match_body(Matcher::Regex("\"bracket1\"".to_owned()))
        .with_status(200)
        .with_body(
            json!({
                "failureReason": "Success",
                "orderId": 5002,
                "oso1Id": 6001
            })
            .to_string(),
        )
        .create_async()
        .await;

    let liquidate_mock = server
        .mock("POST", "/v1/order/liquidateposition")
        .match_header("authorization", "Bearer execution-token")
        .match_body(Matcher::Regex("\"contractId\":4444".to_owned()))
        .match_body(Matcher::Regex("\"customTag50\":\"flatten\"".to_owned()))
        .with_status(200)
        .with_body(json!({"failureReason": "Success", "orderId": 5003}).to_string())
        .create_async()
        .await;

    let order_result = client
        .place_order(TradovatePlaceOrderRequest {
            context: context.clone(),
            order: TradovateOrderPlacement {
                symbol: "GCM6".to_owned(),
                side: TradeSide::Buy,
                quantity: 1,
                order_type: TradovateOrderType::Limit,
                limit_price: Some(Decimal::new(238_510, 2)),
                stop_price: None,
                time_in_force: Some(TradovateTimeInForce::Day),
                expire_time: None,
                text: Some("entry".to_owned()),
                activation_time: None,
                custom_tag_50: Some("bot".to_owned()),
                is_automated: true,
            },
        })
        .await
        .expect("place order should succeed");

    let oso_result = client
        .place_oso(TradovatePlaceOsoRequest {
            context: context.clone(),
            order: TradovateOsoOrderPlacement {
                symbol: "GCM6".to_owned(),
                side: TradeSide::Buy,
                quantity: 1,
                order_type: TradovateOrderType::Limit,
                limit_price: Some(Decimal::new(238_510, 2)),
                stop_price: None,
                time_in_force: Some(TradovateTimeInForce::Day),
                expire_time: None,
                text: Some("entry-oso".to_owned()),
                activation_time: None,
                custom_tag_50: Some("bot".to_owned()),
                is_automated: true,
                brackets: vec![TradovateBracketOrder {
                    side: TradeSide::Sell,
                    quantity: None,
                    order_type: TradovateOrderType::Stop,
                    limit_price: None,
                    stop_price: Some(Decimal::new(237_500, 2)),
                    time_in_force: Some(TradovateTimeInForce::Gtc),
                    expire_time: None,
                    text: Some("stop".to_owned()),
                    activation_time: None,
                    custom_tag_50: None,
                }],
            },
        })
        .await
        .expect("place oso should succeed");

    let liquidation_result = client
        .liquidate_position(TradovateLiquidatePositionRequest {
            context,
            contract_id: 4444,
            custom_tag_50: Some("flatten".to_owned()),
            admin: false,
        })
        .await
        .expect("liquidation should succeed");

    place_order_mock.assert_async().await;
    place_oso_mock.assert_async().await;
    liquidate_mock.assert_async().await;

    assert_eq!(order_result.order_id, 5001);
    assert_eq!(oso_result.order_id, 5002);
    assert_eq!(oso_result.oso1_id, Some(6001));
    assert_eq!(liquidation_result.order_id, 5003);
}
