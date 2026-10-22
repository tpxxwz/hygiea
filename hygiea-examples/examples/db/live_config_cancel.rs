//! 按动态配置取消一轮工作：开关关了、时间段到点，正在跑的那一轮马上停下。
//!
//! 配置类型实现 `CancelCondition`，struct 上写 `#[hy_live_config(cancel_condition)]`，生成的句柄多一个 `cancel_token`：
//! 从上级 token（比如服务的退出信号）得出一个子 token，用它包住一轮工作，取消了这一轮的 future 被丢掉，
//! 正在等的 sleep、请求也马上停。这里不经过 Registry，用 `DbResources::from_config` 直接建一个 SQLite 连接池组件的资源
//! （`uses = 3`：业务池 + 动态配置，业务池拿来模拟别的程序改表），自己跑完退出。
//! 判断规则要用到配置以外的状态时（比如这一轮开始时在不在时间段里），用句柄上的 `cancel_token_with` 由调用方给规则，这里不演示。
//!
//! 1. 配置 `job`：`start` 开关、`every_day_range` 每天哪段时间跑。`should_cancel`：开关关了、或者不在时间段里；
//!    `recheck_after`：离时间段结束还有多久，配置不变到点也会再判断一次
//! 2. 第一轮：跑到一半改表把 `start` 关掉，一个轮询间隔内（SQLite 默认 1 秒）这一轮停下
//! 3. 第二轮：改表把开关打开、时间段的结束改成几秒后，之后不再改表，到了结束时间这一轮停下（靠 `recheck_after`）
//!
//! ```bash
//! cargo run -p hygiea-examples --example db_live_config_cancel
//! ```
//!
//! 第二轮的时间段是「现在 + 5 秒」，快到半夜 0 点时跑会跨天，结束时间比开始早，读配置报错。

use std::ops::Range;
use std::time::Duration;

use hygiea::app::{CancellationToken, RegistryConfig};
use hygiea::datetime::{HygieaTimeExt, Time, TimeFormatter, TimeParser, now_time_local};
use hygiea::db::live_config::{CancelCondition, hy_live_config};
use hygiea::db::{DbResources, DbUses, SqliteConnectConfig, SqlxSqliteConfig, SqlxSqlitePool};
use hygiea::{BaseErr, Result, ResultExt, err};
use serde::{Deserialize, Serialize};

/// 一个定时任务的开关：开了 cancel_condition，句柄上多一个 cancel_token
#[derive(Serialize, Deserialize, hy_live_config)]
#[hy_live_config(cancel_condition)]
pub struct JobSwitch {
    pub start: bool,
    /// 存成 `{"start":"00:00:00","end":"23:59:59"}`
    #[serde(with = "hygiea::datetime::hms_range")]
    pub every_day_range: Range<Time>,
}

impl CancelCondition for JobSwitch {
    fn should_cancel(&self) -> bool {
        !self.start || !self.every_day_range.contains(&now_time_local())
    }

    /// 时间段配置不变也会到点结束：到结束时间再判断一次
    fn recheck_after(&self) -> Option<Duration> {
        Some((self.every_day_range.end - now_time_local()).unsigned_abs())
    }
}

/// 表里没有的 key 用这里的值：开着，全天
impl JobSwitch {
    fn new() -> Result<Self> {
        Ok(Self {
            start: true,
            every_day_range: Time::parse_ext("00:00:00", TimeParser::HMS)?
                ..Time::parse_ext("23:59:59", TimeParser::HMS)?,
        })
    }
}

/// 一轮工作：10 步，每步 1 秒。被取消时停在正在等的那一步
async fn work(round: u32) -> Result<()> {
    for step in 1..=10 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        println!("第 {round} 轮：第 {step} 步做完");
    }
    Ok(())
}

/// 跑一轮：用按配置得出的子 token 包住，取消了 work 的 future 被丢掉
async fn run_round(
    switch: &JobSwitchLiveConfig,
    shutdown: &CancellationToken,
    round: u32,
) -> Result<()> {
    let cancel = switch.cancel_token(shutdown);
    match cancel.token().run_until_cancelled(work(round)).await {
        Some(result) => result,
        None => {
            println!("第 {round} 轮：按配置取消了");
            Ok(())
        }
    }
}

/// 模拟别的程序改表：就是一条普通的 UPDATE，动态配置那边靠轮询发现
async fn set(pool: &SqlxSqlitePool, key: &str, value: &str) -> Result<()> {
    sqlx::query("UPDATE hygiea_config SET value = ?1 WHERE namespace = 'job' AND key = ?2")
        .bind(value)
        .bind(key)
        .execute(&**pool)
        .await
        .wrap_err(|| err!(BaseErr::SysErr))?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // 每次从空库开始：动态配置另开一条连接，要用文件库（内存库每个连接各是一个库）
    let dir = std::env::temp_dir().join("hygiea_live_config_cancel_example");
    std::fs::create_dir_all(&dir).wrap_err(|| err!(BaseErr::SysErr))?;
    let path = dir.join(format!("{}.db", std::process::id()));
    let config = SqlxSqliteConfig {
        connect: SqliteConnectConfig {
            database: path.display().to_string(),
            ..Default::default()
        },
        uses: DbUses::try_from(DbUses::POOL | DbUses::LIVE_CONFIG)?,
        ..Default::default()
    };
    let resources = DbResources::from_config(&config, &RegistryConfig::default()).await?;
    let (Some(pool), Some(source)) = (&resources.pool, &resources.live_config) else {
        return Err(err!(BaseErr::SysErr));
    };
    let job = source.bind("job", JobSwitch::new()?).await?;
    let switch = job.fields();
    // 服务的退出信号，这里没人取消它
    let shutdown = CancellationToken::new();

    // 第一轮：3 秒后关掉开关
    let pool_for_off = pool.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(3)).await;
        println!("改表：start = false");
        if let Err(e) = set(&pool_for_off, "start", "false").await {
            eprintln!("改表失败：{e:#}");
        }
    });
    run_round(&switch, &shutdown, 1).await?;

    // 第二轮：打开开关，时间段的结束改成 5 秒后，之后不再改表
    let end = (now_time_local() + Duration::from_secs(5)).format_ext(TimeFormatter::HMS)?;
    println!("改表：start = true，every_day_range 到 {end} 结束");
    set(
        pool,
        "every_day_range",
        &format!(r#"{{"start":"00:00:00","end":"{end}"}}"#),
    )
    .await?;
    set(pool, "start", "true").await?;
    let mut watcher = job.clone();
    while !watcher.get().start {
        watcher.changed().await?;
    }
    run_round(&switch, &shutdown, 2).await?;

    resources.close().await?;
    std::fs::remove_file(&path).wrap_err(|| err!(BaseErr::SysErr))?;
    Ok(())
}
