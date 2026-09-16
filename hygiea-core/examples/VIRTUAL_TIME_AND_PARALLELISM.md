# Tokio 虚拟时间 & 多线程并行 — 调研笔记

> 配合 `examples/backtest_parallel.rs` 阅读。本文整理 tokio 时间 API、虚拟时钟机制、多线程下的限制，以及"全局虚拟时钟"在 Rust 异步生态下的几种实现路径。

---

## 目录

1. [tokio::time 完整 API 清单](#1-tokiotime-完整-api-清单)
2. [虚拟时间机制：start_paused / pause / advance](#2-虚拟时间机制start_paused--pause--advance)
3. [多线程限制：为什么 multi_thread 禁止 pause](#3-多线程限制为什么-multi_thread-禁止-pause)
4. [跨线程共享虚拟时钟的几种方案](#4-跨线程共享虚拟时钟的几种方案)
5. [madsim 仿真框架详解](#5-madsim-仿真框架详解)
6. [方案对比表](#6-方案对比表)
7. [决策树](#7-决策树)
8. [参考链接](#8-参考链接)

---

## 1. tokio::time 完整 API 清单

### 1.1 等待 / 延迟

| API | 含义 |
|---|---|
| `sleep(Duration)` | 等指定**相对**时长 |
| `sleep_until(Instant)` | 等到**绝对**时刻 |

### 1.2 超时

| API | 含义 |
|---|---|
| `timeout(Duration, future)` | 相对超时（最多等 N 秒） |
| `timeout_at(Instant, future)` | 绝对超时（最晚到某时刻） |

### 1.3 周期触发器

| API | 含义 |
|---|---|
| `interval(period)` | 从**当前时刻**开始，每隔 period 触发一次 |
| `interval_at(start, period)` | 从**指定时刻**开始，每隔 period 触发一次 |

### 1.4 虚拟时间控制（测试 / 仿真专用）

| API | 含义 |
|---|---|
| `pause()` | 运行时**暂停**虚拟时间 |
| `resume()` | 恢复让虚拟时间正常流逝 |
| `advance(Duration)` | 手动推进虚拟时间 |

### 1.5 类型

| 类型 | 用途 |
|---|---|
| `Instant` | 单调时钟时间点。`Instant::now()` 拿当前；做 `sleep_until`、`timeout_at` 的参数 |
| `Duration` | 时长（re-export `std::time::Duration`） |
| `Sleep` | `sleep()` 返回的 future |
| `Timeout<T>` | `timeout()` 返回的 future |
| `Interval` | `interval()` 返回的对象，调 `.tick()` 拿下一个时刻 |
| `MissedTickBehavior` | `Burst`(默认) / `Delay` / `Skip` —— 错过 tick 的补偿策略 |

### 1.6 错误

| 错误 | 含义 |
|---|---|
| `error::Elapsed` | `timeout` 超时返回的错误 |
| `error::Error` | timer 模块通用错误 |

### 1.7 容易被忽略的进阶 API

- **`Sleep::reset(Instant)`** —— 不新建 Sleep，直接把现有的"睡到 X 时刻"改成"睡到 Y 时刻"。`tokio::select!` 循环里重置 sleep 很常见。
- **`Interval::reset()` / `reset_at()` / `reset_after()`** —— 重新对齐 interval 的下一次触发点。
- **`Interval::set_missed_tick_behavior(MissedTickBehavior)`** —— 切换错过 tick 的处理策略。
- **`Instant::now()`** —— 虚拟时间下返回**虚拟**当前；真实时间下返回真实时刻。这就是 `sleep_until` 能跨真实/虚拟时间一致工作的原因。

> 完整文档：<https://docs.rs/tokio/latest/tokio/time/index.html>

---

## 2. 虚拟时间机制：start_paused / pause / advance

### 2.1 工作原理

tokio 的虚拟时钟存在 **runtime 内部**，是 runtime-local 的状态（不是全局也不是 thread_local）。

```
Runtime 实例
├─ Timer wheel (注册的 sleep / interval 定时器)
├─ Virtual clock (当前虚拟时刻)
└─ Task 调度器
```

- `pause()` 把 runtime 内部的 "时间是否流逝" 标记位翻成 false
- `advance(d)` 手动把虚拟时刻往前推 d，并触发到期的定时器 wake 对应 task
- `Instant::now()` 在 paused 模式下返回虚拟时刻；非 paused 下返回真实墙钟时刻
- `tokio::time::sleep` 内部记录"在虚拟时刻 X 时唤醒我"，所以 paused 下不消耗墙钟时间

### 2.2 三个使用入口

**a. 测试**

```rust
#[tokio::test(start_paused = true)]
async fn xxx() {
    tokio::time::advance(Duration::from_secs(10)).await;
}
```

**b. 自定义 runtime（example 里就是这种）**

```rust
let rt = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .start_paused(true)
    .build()
    .unwrap();
rt.block_on(async { ... });
```

**c. 运行时切换**

```rust
#[tokio::main(flavor = "current_thread")]
async fn main() {
    tokio::time::pause();
    // ... 业务逻辑
    tokio::time::resume();  // 切回真实时钟
    tokio::time::sleep(Duration::from_millis(50)).await;  // 真等 50ms
    tokio::time::pause();
}
```

### 2.3 paused 下哪些东西冻结 / 哪些没冻结

**冻结（受控）：**
- `tokio::time::sleep` / `sleep_until`
- `tokio::time::interval` / `interval_at`
- `tokio::time::timeout` / `timeout_at`
- `tokio::time::Instant::now()`

**不冻结（继续走真实时间）：**
- `std::time::Instant::now()` — 墙钟
- `std::time::SystemTime::now()` — 系统时间
- `chrono::Utc::now()` — 日历时间
- 实际 I/O：网络请求、磁盘读写
- 其他线程的 `std::thread::sleep`
- 计算耗时

### 2.4 隐蔽陷阱

只要库底层用了 `tokio::time`，就会被 pause 影响：

```rust
#[tokio::main(flavor = "current_thread")]
async fn main() {
    tokio::time::pause();
    some_library::do_with_retry().await;  // 内部用了 tokio::time::sleep
    // ↑ 永远卡住，它的 sleep 等不到
}
```

这是 **共享代码两面性**：
- ✅ 仿真/回测想让"重试、超时"也时间穿越 → 利用这一点
- ❌ 不小心在生产代码里 pause → 整个进程僵死

---

## 3. 多线程限制：为什么 multi_thread 禁止 pause

### 3.1 现象

```rust
#[tokio::main]  // 默认 multi_thread
async fn main() {
    tokio::time::pause();   // ❌ panic
}
```

panic 信息：`"time cannot be paused while multi-thread runtime is running"`

源码层面在 `tokio::time::pause()` 入口处直接检查 runtime 类型，发现 multi_thread 立即 panic。**绕不过去**（除非 fork tokio）。

### 3.2 设计原因

多线程虚拟时间在语义上有几个无解问题：

1. **advance 谁来推进、谁该等？**
   - 时间是全局共享状态，多 worker 并发读"现在几点"，没法保证一致快照。

2. **唤醒定时器的雪崩**
   - `advance(1 小时)` 可能瞬间唤醒上万 sleep 定时器，多 worker 同时分发 → 不同 worker 看到不同的时间值。

3. **死锁风险**
   - 一个 worker advance 时遍历定时器堆 wake task，其他 worker 上的 task 已经醒来抢同一把时间锁。

4. **测试可重现性丧失**
   - 虚拟时间的意义就是确定性；多线程加进来 task 调度顺序又变得不确定。

tokio 选择"current_thread + 虚拟时间换确定性，multi_thread + 真实时间换吞吐"，两者不混。

---

## 4. 跨线程共享虚拟时钟的几种方案

如果项目需要"多线程并行 + 受控时钟"，有以下几条路。

### 方案 A：每线程独立 paused runtime

**思路**：外层 `std::thread` / `rayon`，每个 OS 线程自己 build 一个 `current_thread + start_paused` 的 runtime。线程之间时钟互相独立。

**适用**：参数搜索、并行回测 —— 各回测会话独立完成，无需共享时钟。

**代码**：见 `examples/backtest_parallel.rs`。

**能力 / 限制**：
- ✅ 利用所有 CPU 核心
- ✅ 业务代码零改动，照常用 `tokio::time::*`
- ✅ 每个 runtime 独立时钟，互不串扰
- ❌ 线程之间**无法共享时钟**（这是 by-design）
- ❌ 单个回测会话内部仍然是单线程（async 协作调度）

### 方案 B：自建全局 Clock + 自建 sleep（脱离 tokio::time）

**思路**：自己写一个 `Clock` 抽象，业务代码不再调 `tokio::time::sleep`，改调 `clock.sleep_until`。底层基于 `tokio::sync::Notify` 实现。

**示例**：

```rust
use std::sync::Arc;
use parking_lot::Mutex;
use tokio::sync::Notify;

#[derive(Clone)]
struct SharedClock {
    inner: Arc<Inner>,
}

struct Inner {
    now: Mutex<u64>,     // 毫秒级虚拟时刻
    notify: Notify,
}

impl SharedClock {
    fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                now: Mutex::new(0),
                notify: Notify::new(),
            }),
        }
    }

    fn now(&self) -> u64 {
        *self.inner.now.lock()
    }

    fn advance(&self, delta: u64) {
        *self.inner.now.lock() += delta;
        self.inner.notify.notify_waiters();  // 唤醒所有 sleep
    }

    async fn sleep_until(&self, target: u64) {
        loop {
            if self.now() >= target {
                return;
            }
            self.inner.notify.notified().await;
        }
    }
}
```

**能力 / 限制**：
- ✅ 真正跨线程共享时钟，可以用 `multi_thread` runtime
- ✅ Controller 推进 + Worker 跑业务的语义可以实现
- ❌ 业务代码全部改造 —— 不能再用 `tokio::time::sleep / interval / timeout`
- ❌ 第三方库内部的 `tokio::time::sleep`（reqwest 重试、tower timeout）**不受控**
- ❌ `chrono::Utc::now()` 也不受控（要么也走 clock 接口，要么混用就乱）
- ❌ 相当于自建一套时间生态，工程量大

### 方案 C：多进程隔离

**思路**：每个回测/会话开一个子进程，进程间天然隔离 static 状态、tokio runtime、时钟。

```rust
let children: Vec<_> = param_grid.iter().map(|p| {
    Command::new(std::env::current_exe().unwrap())
        .args(["--backtest", &serde_json::to_string(p).unwrap()])
        .spawn().unwrap()
}).collect();
for mut c in children { c.wait().unwrap(); }
```

**能力 / 限制**：
- ✅ 进程级隔离最彻底，崩一个不影响别的
- ✅ 业务代码零改动
- ✅ 自动用满多核
- ❌ 进程启动开销
- ❌ 结果要序列化回传
- ❌ 调试稍麻烦（多进程 attach）

### 方案 D：madsim 仿真框架

见下一节。

### 方案 E：Fork tokio 改源码

理论可行，但：
- 要改 `tokio/src/time/clock.rs` 移除 multi_thread 检查
- 要重写 timer wheel 让它线程安全跨 worker
- 要解决死锁、一致性、雪崩问题
- 维护成本 = 自己维护一个 tokio 分支
- 升级 tokio 时手动 merge

工业界已知没人这么干。

---

## 5. madsim 仿真框架详解

### 5.1 是什么

[madsim](https://github.com/madsim-rs/madsim) 是蚂蚁集团 / RisingWave 开发的**确定性仿真框架**，专门解决"多节点 + 全局虚拟时间 + 网络模拟"。

广泛用于：
- [RisingWave](https://github.com/risingwavelabs/risingwave)（流式数据库）
- [TiKV](https://github.com/tikv/tikv) 的部分测试
- 分布式共识算法验证

### 5.2 工作原理：通过 cargo cfg 替换 tokio

madsim **不是和 tokio 配合使用，而是替代 tokio**。通过 cargo 的 target_cfg 机制：

**Cargo.toml**：
```toml
[dependencies]
tokio = "1"

[target.'cfg(madsim)'.dependencies]
tokio = { version = "1", package = "madsim-tokio" }
```

**编译开关**：
```bash
# 生产 build：用真实 tokio
cargo build --release

# 仿真 build：tokio 被替换成 madsim 兼容层，虚拟时间生效
RUSTFLAGS="--cfg madsim" cargo build
```

业务代码**完全不变**，照常 `use tokio::time::sleep`。在两种编译模式下指向不同的实现。

### 5.3 API 兼容性

madsim 实现了 tokio 大部分子模块：

| tokio 模块 | madsim 是否提供 |
|---|---|
| `tokio::time` | ✅ 全部（sleep / interval / timeout / Instant） |
| `tokio::sync` | ✅ Mutex / RwLock / Notify / mpsc / oneshot / broadcast |
| `tokio::task` | ✅ spawn / yield_now / JoinHandle |
| `tokio::net` | ✅ TcpListener / TcpStream / UdpSocket（模拟网络） |
| `tokio::io` | ✅ AsyncRead / AsyncWrite |
| `tokio::fs` | ⚠️ 部分（文件系统不模拟，直接转发到真实） |
| `tokio::signal` | ❌ |
| `tokio::process` | ❌ |

### 5.4 仿真模式下的能力

```rust
#[madsim::main]
async fn main() {
    let handle = madsim::Handle::current();

    // 创建多个"逻辑节点"，每个节点独立内存、网络隔离
    let node1 = handle.create_node().name("trader-1").build();
    let node2 = handle.create_node().name("trader-2").build();

    node1.spawn(async {
        tokio::time::sleep(Duration::from_secs(5)).await;  // 全局共享虚拟时钟
        send_order().await;
    });

    node2.spawn(async {
        tokio::time::sleep(Duration::from_secs(3)).await;
        receive_order().await;
    });
}
```

特性清单：
- **全局虚拟时钟**：所有节点共享同一时间轴
- **确定性调度**：固定随机种子 → 跑 1000 次结果完全一致
- **网络模拟**：注入延迟、丢包、分区
- **节点故障注入**：crash / restart / 网络隔离
- **panic 复现**：测试发现 bug，同样 seed 完美复现

### 5.5 限制

- **整个仿真进程内单线程跑**（这是确定性的代价）
- 跨参数并行需要外层多进程
- 第三方库要 madsim 兼容（生态相对小，主要是数据库 / 分布式工具）
- 学习曲线：Node API、seed 控制、cfg 切换
- 调试工具不如直接 tokio 完善

---

## 6. 方案对比表

| 维度 | A: 每线程独立 paused | B: 自建 Clock | C: 多进程 | D: madsim | E: fork tokio |
|---|---|---|---|---|---|
| 跨线程共享时钟 | ❌ | ✅ | ❌ | ✅（节点间） | 取决于实现 |
| 多核并行 | ✅ | ✅ | ✅ | ⚠️ 仿真内单线程，需外层多进程 | ✅ |
| 业务代码改动 | 0 | 大 | 0 | 极小（cfg + Cargo.toml） | 0 |
| 第三方库 `tokio::time` 受控 | ✅（runtime 内） | ❌ | ✅（进程内） | ✅（替换 tokio） | ✅ |
| 网络 / 故障模拟 | ❌ | 自己写 | ❌ | ✅ | 自己写 |
| 确定性可重现 | ⚠️ 取决于策略代码 | 自己保证 | ⚠️ | ✅ 核心卖点 | 自己保证 |
| 工程量 | 极小 | 大 | 小 | 中（学习 + cfg 配置） | 极大 |
| 生态成熟度 | ✅ tokio 官方 | 自建 | ✅ OS 级 | ⚠️ 主要在数据库 / 分布式领域 | ❌ |
| 调试难度 | 低 | 中 | 中 | 中-高 | 高 |

---

## 7. 决策树

```
1. 跨线程是否需要共享同一虚拟时钟？
   │
   ├─ 否（参数级并行、各回测独立）
   │   └─ → 方案 A（每线程独立 paused runtime）
   │
   └─ 是（多节点协同、需要全局时间轴）
       │
       ├─ 是否需要网络 / 故障 / 多节点模拟？
       │   ├─ 是 → 方案 D（madsim）
       │   └─ 否 → 继续问
       │
       └─ 业务代码是否能接受全面改造？
           ├─ 能 → 方案 B（自建 Clock）
           └─ 不能 → 方案 C（多进程） 或 方案 D（madsim）
```

---

## 8. 参考链接

- tokio time 模块文档：<https://docs.rs/tokio/latest/tokio/time/index.html>
- tokio runtime 配置：<https://docs.rs/tokio/latest/tokio/runtime/struct.Builder.html>
- tokio test 宏：<https://docs.rs/tokio/latest/tokio/attr.test.html>
- madsim 项目主页：<https://github.com/madsim-rs/madsim>
- madsim 实战项目（RisingWave）：<https://github.com/risingwavelabs/risingwave>
- TiKV 的 madsim 集成：<https://github.com/tikv/tikv>
- 协作式调度 + cooperative budget：<https://docs.rs/tokio/latest/tokio/task/fn.consume_budget.html>

---

## 附：本仓库相关代码索引

| 文件 | 内容 |
|---|---|
| `hygiea-core/src/sim.rs` | `SimClock` + `TokenBucket` 实现，配套测试 |
| `hygiea-core/src/sim.rs` 内 `tests::tokio_*` | tokio::time 各 API 的虚拟时间演示测试 |
| `hygiea-core/examples/backtest_parallel.rs` | 方案 A 的完整 example：std::thread + 独立 paused runtime |
