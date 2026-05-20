//! 并行回测示例：多线程 + 虚拟时间
//!
//! ## 背景
//!
//! `tokio::time::pause()` 只能在 **current_thread runtime**（单线程）上使用。
//! 在 `multi_thread` runtime 上直接 panic（tokio 源码层面禁止）。
//!
//! 那么"想在多核 CPU 上并行跑回测、每个回测内部又要用虚拟时间"怎么办？
//!
//! ## 思路
//!
//! - **外层并行**：每个参数组合一个 OS 线程（用 std::thread 或 rayon）
//! - **内层单线程**：每个线程自己 `Builder::new_current_thread().start_paused(true)`
//!   创建独立的 paused runtime，互不干扰
//! - **结果**：所有 CPU 核心打满 + 每个回测会话内部确定性时间控制
//!
//! ## 运行
//!
//! ```
//! cargo run --example backtest_parallel -p hygiea-core --release
//! ```

use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tokio::time::{advance, sleep, timeout, Instant};

// ============================================================================
// 模拟领域类型（真实项目中是你自己的 Kline / Strategy / Order 等）
// ============================================================================

/// 一根 K 线（简化版）
#[derive(Debug, Clone)]
struct Kline {
    /// 距离回测起点的偏移（秒）。真实代码里是 chrono::DateTime<Utc>
    timestamp_sec: u64,
    open: f64,
    close: f64,
}

/// 策略参数 —— 一次回测的输入
#[derive(Debug, Clone)]
#[allow(dead_code)] // 示例里只用到部分字段，真实场景每个字段都会被用上
struct Params {
    id: u32,
    /// 均线窗口
    ma_window: usize,
    /// 入场阈值（涨幅 > threshold 时买）
    threshold: f64,
}

/// 一次回测的结果
#[derive(Debug)]
struct BacktestResult {
    params_id: u32,
    bars_processed: usize,
    trades: usize,
    final_pnl: f64,
}

// ============================================================================
// 准备阶段：装载市场数据（所有线程共享只读）
// ============================================================================

fn load_market_data() -> Arc<Vec<Kline>> {
    // 真实场景：从 parquet / csv / 数据库读取百万根 K 线
    // 这里造 20 根演示，每根间隔 60 秒
    let klines: Vec<Kline> = (0..20)
        .map(|i| Kline {
            timestamp_sec: i * 60,
            open: 100.0 + (i as f64) * 0.1,
            // 偶数根上涨，奇数根下跌（制造一点交易信号）
            close: 100.0 + (i as f64) * 0.1 + if i % 2 == 0 { 0.5 } else { -0.3 },
        })
        .collect();
    Arc::new(klines)
}

// ============================================================================
// 内层：单次回测（async 函数，跑在 current_thread runtime 上）
// ============================================================================

/// 跑一次回测
///
/// 这个函数演示几个 `tokio::time` 关键 API 在 paused 模式下的行为：
///
/// - [`tokio::time::advance`]: 主动推进虚拟时钟。回测核心驱动力。
/// - [`tokio::time::sleep`]: 策略代码里的"等 N 秒"。paused 下不会真的等。
/// - [`tokio::time::timeout`]: 给某个 await 加超时。也是基于虚拟时间。
/// - [`tokio::time::Instant::now`]: 返回当前**虚拟**时刻。
async fn run_backtest(params: Params, market_data: Arc<Vec<Kline>>) -> BacktestResult {
    // 记录回测起点（虚拟时刻 = 0）
    let start = Instant::now();
    let mut trades = 0;

    for kline in market_data.iter() {
        // 1) 推进虚拟时钟到当前 K 线的时刻
        //    advance(delta) 会唤醒所有在此期间到期的 sleep / timer。
        //    回测引擎的 "时间引擎" 本质就是这一行。
        let target = start + Duration::from_secs(kline.timestamp_sec);
        let now = Instant::now();
        if target > now {
            advance(target - now).await;
        }

        // 2) 策略逻辑：涨幅超过阈值就"下单"
        let gain = kline.close - kline.open;
        if gain > params.threshold {
            trades += 1;

            // 3) 演示 sleep：假设策略代码下单后要等 1s 等待撮合确认
            //    paused 模式下这个 sleep **不消耗墙钟时间**，但仍然是
            //    协作让出点，可以被 select! / timeout 抢断。
            sleep(Duration::from_millis(500)).await;

            // 4) 演示 timeout：给"模拟撮合"加一个 2s 超时
            //    （这里 inner 立即完成，不会真触发超时）
            let _ = timeout(Duration::from_secs(2), async {
                // 假装这里是撮合逻辑
                tokio::task::yield_now().await;
                Ok::<(), ()>(())
            })
            .await;
        }
    }

    BacktestResult {
        params_id: params.id,
        bars_processed: market_data.len(),
        trades,
        final_pnl: trades as f64 * 0.1,
    }
}

// ============================================================================
// 桥接层：在单个 OS 线程内创建独立 runtime 跑一次回测
// ============================================================================

/// 在当前 OS 线程上创建一个专用的 current_thread runtime + 暂停时钟，
/// 然后 block_on 跑完一次回测。
///
/// 这是整个方案的关键：
/// - 每个 OS 线程**独占**一个 runtime
/// - 每个 runtime 有**独立的虚拟时钟**（互不干扰）
/// - 多个线程并行 = 多个回测同时进行
fn run_in_dedicated_runtime(params: Params, market_data: Arc<Vec<Kline>>) -> BacktestResult {
    // Builder 配置说明：
    //
    // .new_current_thread()
    //   单线程 runtime。pause/resume/start_paused 只能用在这上面。
    //   多线程 runtime 调用 pause() 会 panic。
    //
    // .enable_all()
    //   打开 time / io 等所有驱动。否则 sleep/timeout/interval 不工作。
    //   也可以只 enable_time() 节省一点点开销，但 enable_all 最省事。
    //
    // .start_paused(true)
    //   runtime 启动后立刻调用 pause()，进入虚拟时间模式。
    //   等价于 build 完之后手动 rt.block_on(async { tokio::time::pause() })。
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .expect("build runtime failed");

    // block_on 把 async 任务跑到结束。
    // 这一行**同步阻塞**当前 OS 线程，直到 future 返回。
    // 在该期间，当前线程只会处理这个 runtime 调度的 task。
    rt.block_on(run_backtest(params, market_data))
}

// ============================================================================
// 外层：参数网格 + 并行调度
// ============================================================================

fn main() {
    // 1) 装载市场数据（一次性，所有 worker 共享只读）
    //    用 Arc 包起来，clone 只增加引用计数，不拷贝底层 Vec
    let market_data = load_market_data();

    // 2) 构造参数网格
    //    真实项目里可能是几千上万个组合（grid search / Bayes opt / 遗传算法）
    let param_grid: Vec<Params> = (0..8)
        .map(|i| Params {
            id: i,
            ma_window: 5 + i as usize,
            threshold: 0.05 + 0.05 * i as f64,
        })
        .collect();

    println!(
        "Spawning {} worker threads (each runs its own paused runtime)...",
        param_grid.len()
    );

    // 3) 每个参数组合 spawn 一个 OS 线程
    //
    //    ⚠️ 生产中建议：
    //    - 用 rayon::par_iter 或自建线程池，避免一次 spawn 几千线程
    //    - 用 num_cpus::get() 控制并发度（线程数 = CPU 核心数）
    //
    //    这里为了演示纯粹，用 std::thread::spawn。8 个线程问题不大。
    let handles: Vec<_> = param_grid
        .into_iter()
        .map(|params| {
            // Arc::clone 是 O(1)，只增加引用计数
            let data = market_data.clone();
            thread::spawn(move || {
                let thread_id = thread::current().id();
                println!("  [worker {:?}] start params id={}", thread_id, params.id);
                let result = run_in_dedicated_runtime(params, data);
                println!("  [worker {:?}] done   params id={}", thread_id, result.params_id);
                result
            })
        })
        .collect();

    // 4) join 所有 worker，收集结果
    let mut results: Vec<BacktestResult> = handles
        .into_iter()
        .map(|h| h.join().expect("worker thread panicked"))
        .collect();

    // 5) 排序输出（按 pnl 降序）
    results.sort_by(|a, b| b.final_pnl.partial_cmp(&a.final_pnl).unwrap());

    println!("\n=== Results (sorted by pnl) ===");
    for r in &results {
        println!(
            "  params_id={:>2}  bars={:>3}  trades={:>3}  pnl={:>6.2}",
            r.params_id, r.bars_processed, r.trades, r.final_pnl
        );
    }

    let best = &results[0];
    println!("\nBest params: id={} pnl={:.2}", best.params_id, best.final_pnl);
}
