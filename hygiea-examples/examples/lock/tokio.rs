//! 进程内的锁 `TokioLock`（feature `lock-tokio`）：不连外部服务，同一个进程里的任务互斥，比如单实例部署、本地开发。
//!
//! 1. 业务代码只依赖 `DistributedLock` trait（`settle` 函数），换成 PG 连接池（`db_lock` 示例）不用改
//! 2. 同一个 key 只有一个任务拿得到，别的立即返回 `None`；不同 key 互不影响
//! 3. 阻塞版直接等 Mutex，先到先得，不轮询；等不到超时返回 `None`
//! 4. clone 出来的共用一张表；另外 `default()` 建的是另一张表，互相看不到
//! 5. 整数 key 走 trait 的默认实现，转成字符串 key（`999999999:<数字>`）
//!
//! ```bash
//! cargo run -p hygiea-examples --example lock_tokio
//! ```

use std::time::Duration;

use hygiea::Result;
use hygiea::lock::{DistributedLock, TokioLock, hy_lock_key, hy_lock_type, lock_key};

/// 编号 = 项目前缀（Cargo.toml 的 `err_code_project_prefix`，这里是 001）+ 模块前缀 + code
#[derive(hy_lock_key)]
#[lock_module_prefix = "02"]
enum JobLockKey {
    #[lock_key(code = "0001", tpl = "settle:{{ day }}")]
    Settle,
}

#[derive(hy_lock_type)]
#[lock_module_prefix = "02"]
enum JobLockType {
    #[lock_type(code = "0002")]
    Export,
}

/// 业务代码：只依赖 trait，拿不到锁就跳过（别的任务正在结算这一天）
async fn settle(lock: &impl DistributedLock, worker: u32, day: &str) -> Result<()> {
    let Some(guard) = lock.try_lock(lock_key!(JobLockKey::Settle, day)).await? else {
        println!("worker {worker}: {day} 正在被别的任务结算，跳过");
        return Ok(());
    };
    println!("worker {worker}: 拿到锁 {}，开始结算", guard.key());
    tokio::time::sleep(Duration::from_millis(200)).await;
    guard.release().await
}

#[tokio::main]
async fn main() -> Result<()> {
    let lock = TokioLock::default();

    // ---- 3 个任务同时结算同一天：只有一个拿得到 ----
    let mut tasks = Vec::new();
    for worker in 1..=3 {
        let lock = lock.clone(); // clone 出来的共用一张表
        tasks.push(tokio::spawn(async move {
            settle(&lock, worker, "2026-10-13").await
        }));
    }
    for task in tasks {
        if let Ok(result) = task.await {
            result?;
        }
    }
    // 不同的 key（另一天）互不影响
    settle(&lock, 4, "2026-10-14").await?;

    // ---- 阻塞版：先到先得，持有的一方 300ms 后释放 ----
    let held = lock
        .try_lock(lock_key!(JobLockKey::Settle, "2026-10-15"))
        .await?;
    println!("\n主任务持有 settle:2026-10-15：{}", held.is_some());
    let waiter = {
        let lock = lock.clone();
        tokio::spawn(async move {
            let start = tokio::time::Instant::now();
            let got = lock
                .lock(
                    lock_key!(JobLockKey::Settle, "2026-10-15"),
                    Duration::from_secs(3),
                )
                .await;
            (got.map(|g| g.is_some()), start.elapsed())
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    if let Some(held) = held {
        held.release().await?;
    }
    if let Ok((got, waited)) = waiter.await {
        println!("等待的任务拿到：{}，等了 {waited:?}", got?);
    }
    let blocker = lock
        .try_lock(lock_key!(JobLockKey::Settle, "2026-10-16"))
        .await?;
    let timed_out = lock
        .lock(
            lock_key!(JobLockKey::Settle, "2026-10-16"),
            Duration::from_millis(100),
        )
        .await?;
    println!("一直有人持有时等 100ms：{}", timed_out.is_some());
    drop(blocker);

    // ---- 另外建的 TokioLock 是另一张表：看不到上面的锁 ----
    let held = lock
        .try_lock(lock_key!(JobLockKey::Settle, "2026-10-17"))
        .await?;
    let other = TokioLock::default();
    let seen = other
        .try_lock(lock_key!(JobLockKey::Settle, "2026-10-17"))
        .await?;
    println!(
        "\n同一个 key，另一个 TokioLock 也拿得到：{}（不是同一张表）",
        held.is_some() && seen.is_some()
    );

    // ---- 整数 key：默认实现转成字符串 key ----
    let by_id = lock.try_lock_i64(42).await?;
    println!("\ni64 key：{:?}", by_id.as_ref().map(|g| g.key()));
    let by_type = lock.try_lock_i32(JobLockType::Export, 7u32).await?;
    println!(
        "锁类型 + i32（合成 i64 再转字符串）：{:?}",
        by_type.as_ref().map(|g| g.key())
    );
    Ok(())
}
