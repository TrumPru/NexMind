/// Live integration test — connects to a real OpenClaw Gateway.
/// Run with:
///   OPENCLAW_GATEWAY_TOKEN=<token> cargo test -p nexmind-openclaw --test live_gateway -- --nocapture
///
/// Requires:
/// - OpenClaw Gateway running on localhost:18789
/// - chatCompletions endpoint enabled in openclaw.json
/// - OPENCLAW_GATEWAY_TOKEN env var set
///
/// Skips automatically if token is not set or gateway is not available.

use nexmind_openclaw::{OpenClawAgent, OpenClawConfig, GatewayClient};

fn test_config() -> Option<OpenClawConfig> {
    let token = std::env::var("OPENCLAW_GATEWAY_TOKEN").ok()?;
    let mut config = OpenClawConfig::local();
    config.gateway_token = Some(token);
    Some(config)
}

async fn gateway_available() -> Option<OpenClawConfig> {
    let config = test_config()?;
    let agent = OpenClawAgent::new(config.clone());
    if agent.is_available().await {
        Some(config)
    } else {
        None
    }
}

#[tokio::test]
async fn test_health_check() {
    let Some(config) = gateway_available().await else {
        println!("⏭️  Skipping: OPENCLAW_GATEWAY_TOKEN not set or gateway unavailable");
        return;
    };

    let agent = OpenClawAgent::new(config);
    let health = agent.health().await;
    assert!(health.is_ok(), "health check should succeed");

    let h = health.unwrap();
    println!("✅ Gateway healthy!");
    println!("   Status:  {:?}", h.status);
    println!("   OK:      {:?}", h.ok);
    assert_eq!(h.status.as_deref(), Some("live"));
}

#[tokio::test]
async fn test_send_message() {
    let Some(config) = gateway_available().await else {
        println!("⏭️  Skipping");
        return;
    };

    let agent = OpenClawAgent::new(config);
    let response = agent.run("Reply with exactly: NEXMIND_TEST_OK").await;

    match response {
        Ok(reply) => {
            println!("✅ Got response from OpenClaw ({} chars)", reply.len());
            println!("   Reply: {}", &reply[..reply.len().min(200)]);
            assert!(!reply.is_empty());
            assert!(reply.contains("NEXMIND_TEST_OK"));
        }
        Err(e) => panic!("❌ Message send failed: {}", e),
    }
}

#[tokio::test]
async fn test_delegate_task() {
    let Some(config) = gateway_available().await else {
        println!("⏭️  Skipping");
        return;
    };

    let agent = OpenClawAgent::new(config);
    let response = agent.delegate_task("What is 2 + 2? Reply with just the number.").await;

    match response {
        Ok(reply) => {
            println!("✅ Task delegated ({} chars): {}", reply.len(), reply);
            assert!(reply.contains('4'));
        }
        Err(e) => panic!("❌ Task delegation failed: {}", e),
    }
}

#[tokio::test]
async fn test_gateway_client_direct() {
    let Some(config) = gateway_available().await else {
        println!("⏭️  Skipping");
        return;
    };

    let client = GatewayClient::new(config);
    let health = client.health_check().await;
    assert!(health.is_ok());
    println!("✅ Direct client health check passed");
}

#[tokio::test]
async fn test_conversation() {
    let Some(config) = gateway_available().await else {
        println!("⏭️  Skipping");
        return;
    };

    let client = GatewayClient::new(config);

    use nexmind_openclaw::gateway::ChatMessage;

    let messages = vec![
        ChatMessage { role: "user".into(), content: "My name is NexMindBot.".into() },
        ChatMessage { role: "assistant".into(), content: "Nice to meet you, NexMindBot!".into() },
        ChatMessage { role: "user".into(), content: "What is my name? Reply with just the name.".into() },
    ];

    let response = client.send_conversation(messages, None, Some("nexmind-test")).await;

    match response {
        Ok(resp) => {
            println!("✅ Conversation: {}", resp.reply);
            assert!(resp.reply.contains("NexMindBot"));
        }
        Err(e) => panic!("❌ Conversation failed: {}", e),
    }
}
