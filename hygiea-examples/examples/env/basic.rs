//! 环境变量：内置 key `BuiltinKey`、自定义 `EnvKey`，以及取值的四个函数
//! `env_get` / `env_get_opt` / `env_get_or` / `env_get_or_else`。
//!
//! ```bash
//! cargo run -p hygiea-examples --example env_basic
//! MY_APP_NAME=hello cargo run -p hygiea-examples --example env_basic
//! ```

use hygiea::Result;
use hygiea::env::{BuiltinKey, EnvKey, env_get, env_get_opt, env_get_or, env_get_or_else};

/// 自定义 key：给应用自己的环境变量用，实现 `EnvKey` 就能用同一套函数取值
struct AppKey(&'static str);

impl EnvKey for AppKey {
    fn key_name(&self) -> &str {
        self.0
    }
}

/// 自定义 key 也可以像 `BuiltinKey` 一样带自己的默认值
struct AppRegionKey;

impl EnvKey for AppRegionKey {
    fn key_name(&self) -> &str {
        "MY_APP_REGION"
    }

    fn default_value(&self) -> Option<String> {
        Some("ap-northeast-1".to_string())
    }
}

fn main() -> Result<()> {
    println!("1. BuiltinKey — 框架内置的几个 key，部分自带默认值:");
    println!(
        "   BuiltinKey::DefaultHome 的默认值: {:?}",
        BuiltinKey::DefaultHome.default_value()
    );
    println!(
        "   本地 Postgres 默认 host:port = {}:{}\n",
        env_get(BuiltinKey::LocalPgHost)?,
        env_get(BuiltinKey::LocalPgPort)?
    );

    println!("2. 自定义 EnvKey，没有默认值：env_get 在环境变量没设时报错:");
    match env_get(AppKey("MY_APP_NAME")) {
        Ok(v) => println!("   MY_APP_NAME = {v}"),
        Err(e) => println!("   env_get 报错: {e}"),
    }
    println!();

    println!("3. 自定义 EnvKey 带默认值：env_get 在环境变量没设时用它:");
    println!("   MY_APP_REGION = {}\n", env_get(AppRegionKey)?);

    println!("4. env_get_opt — 没有就是 None，不看 key 自带的默认值:");
    println!(
        "   MY_APP_REGION（env_get_opt）: {:?}\n",
        env_get_opt(AppRegionKey)
    );

    println!("5. env_get_or — 没有就用调用方给的兜底值:");
    println!(
        "   MY_APP_NAME 或 \"anonymous\": {}\n",
        env_get_or(AppKey("MY_APP_NAME"), "anonymous")
    );

    println!("6. env_get_or_else — 没有才调用闭包算默认值（闭包只在没有时才会跑）:");
    let name = env_get_or_else(AppKey("MY_APP_NAME"), || {
        println!("   (没设 MY_APP_NAME，调用闭包计算默认值)");
        "computed-default".to_string()
    });
    println!("   MY_APP_NAME 或计算值: {name}");

    Ok(())
}
