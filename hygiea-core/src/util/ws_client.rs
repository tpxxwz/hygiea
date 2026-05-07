pub use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Error as WsError,
    tungstenite::Message,
};

#[cfg(test)]
mod tests_subscribe {
    use super::*;
    use crate::EnvKey;
    use futures_util::{SinkExt, StreamExt};

    enum OkxMockEnv {
        ApiKey,
        SecretKey,
    }

    impl EnvKey for OkxMockEnv {
        fn key_name(&self) -> &str {
            match self {
                Self::ApiKey => "OKX_MOCK_API_KEY",
                Self::SecretKey => "OKX_MOCK_SECRET_KEY",
            }
        }
    }

    const OKX_PUBLIC_WS: &str = "wss://wspap.okx.com:8443/ws/v5/public";
    const OKX_PRIVATE_WS: &str = "wss://wspap.okx.com:8443/ws/v5/private";
    const OKX_BUSINESS_WS: &str = "wss://wspap.okx.com:8443/ws/v5/business";

    #[tokio::test]
    #[ignore]
    async fn test_subscribe_ticker() {
        let (mut ws, _) = connect_async(OKX_PUBLIC_WS).await.unwrap();

        let sub = r#"{"op":"subscribe","args":[{"channel":"tickers","instId":"BTC-USDT"}]}"#;
        ws.send(Message::text(sub)).await.unwrap();

        let mut count = 0;
        while let Some(Ok(msg)) = ws.next().await {
            println!("[ticker] {msg}");
            count += 1;
            if count >= 3 {
                break;
            }
        }

        ws.close(None).await.unwrap();
        assert!(count > 0);
    }

    #[tokio::test]
    #[ignore]
    async fn test_subscribe_orderbook() {
        let (mut ws, _) = connect_async(OKX_PUBLIC_WS).await.unwrap();

        let sub = r#"{"op":"subscribe","args":[{"channel":"books5","instId":"ETH-USDT"}]}"#;
        ws.send(Message::text(sub)).await.unwrap();

        let mut count = 0;
        while let Some(Ok(msg)) = ws.next().await {
            println!("[books5] {msg}");
            count += 1;
            if count >= 3 {
                break;
            }
        }

        ws.close(None).await.unwrap();
        assert!(count > 0);
    }
}

#[cfg(test)]
mod tests_ping_pong {
    use super::*;
    use futures_util::{SinkExt, StreamExt};

    const OKX_PUBLIC_WS: &str = "wss://ws.okx.com:8443/ws/v5/public";

    #[tokio::test]
    #[ignore]
    async fn test_ping_pong() {
        let (mut ws, _) = connect_async(OKX_PUBLIC_WS).await.unwrap();

        ws.send(Message::text("ping")).await.unwrap();

        if let Some(Ok(msg)) = ws.next().await {
            println!("[pong] {msg}");
            assert_eq!(msg.to_text().unwrap(), "pong");
        }

        ws.close(None).await.unwrap();
    }

    #[tokio::test]
    #[ignore]
    async fn test_keepalive_multiple_pings() {
        let (mut ws, _) = connect_async(OKX_PUBLIC_WS).await.unwrap();

        for i in 0..3 {
            ws.send(Message::text("ping")).await.unwrap();
            if let Some(Ok(msg)) = ws.next().await {
                println!("[pong #{i}] {msg}");
                assert_eq!(msg.to_text().unwrap(), "pong");
            }
        }

        ws.close(None).await.unwrap();
    }
}
