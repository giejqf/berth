//! The proxy on its own, to try a provider with the `claude` CLI:
//!
//! ```sh
//! OPENAI_API_KEY=… cargo run -p berth-proxy --example serve -- https://api.deepseek.com deepseek-chat
//! ```
//!
//! It prints the environment that points Claude Code at it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use berth_proxy::{Proxy, Upstream};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("berth_proxy=debug")
        .init();
    let mut args = std::env::args().skip(1);
    let base_url = args
        .next()
        .expect("usage: serve <base url> <model> [fast model]");
    let model = args
        .next()
        .expect("usage: serve <base url> <model> [fast model]");
    let fast_model = args.next();
    let proxy = Proxy::start().await.expect("a port on 127.0.0.1");
    proxy.configure(Some(Upstream {
        base_url,
        api_key: std::env::var("OPENAI_API_KEY").ok(),
        models: vec![model.clone()],
        model,
        fast_model,
    }));
    println!(
        "ANTHROPIC_BASE_URL={} ANTHROPIC_AUTH_TOKEN={}",
        proxy.base_url(),
        proxy.token()
    );
    std::future::pending::<()>().await;
}
