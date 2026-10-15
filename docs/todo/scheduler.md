# 定时任务调度待办

做一个定时任务组件：进程内调度，多实例部署时靠数据库协调（和分布式锁、动态配置用同一个库），不额外部署调度中心、Redis、ZooKeeper。
参考 xxl-job（调度中心 + 执行器）和 tokio-cron-scheduler（进程内 cron 调度）。

相关：分布式锁见 `docs/design/lock.md`，动态配置见 `docs/design/live-config.md`，数据库协调能力见 `db-coordination.md`。

## 参考对象

### xxl-job

- 调度中心（独立部署，带管理界面）+ 执行器（业务服务里的 handler），任务定义、触发记录都在调度中心的 MySQL 里
- 调度中心集群靠 `SELECT ... FOR UPDATE` 锁一行做互斥，只有拿到锁的节点扫描"快到期"的任务，算好下次触发时间写回
- 能力：cron / 固定频率 / 手动触发；路由策略（第一个、轮询、一致性哈希、故障转移、**分片广播**）；
  阻塞处理策略（单机串行、丢弃后续调度、覆盖之前调度）；调度过期策略（忽略、立即补一次）；超时、失败重试；
  执行日志、告警；任务依赖（子任务）
- 不要的部分：单独部署的调度中心、执行器注册和心跳、HTTP 回调

### tokio-cron-scheduler

- 一个固定间隔（默认约 500ms）的 tick 循环，扫 job 元数据里 `next_tick <= now` 的，发到 channel，runner `tokio::spawn` 执行
- job 类型：cron（6 段带秒，croner 解析）、固定间隔、延迟一次、指定时刻一次；`add` 返回 `Uuid`，可 `remove`
- 元数据可以存内存、PG、NATS，但只存元数据，**不做多实例互斥**，也不防重入
- 时间类型是 chrono，`chrono-tz` 是必选依赖

### 应用侧最小实现

每个 job 一个 `tokio::spawn` 的循环：`interval` + `MissedTickBehavior::Delay`，`run_once` 跑完才进下一轮（不重入）；
每轮单独 spawn 隔离 panic；开关和参数每轮读动态配置；多实例互斥由任务自己在 `run_once` 里加分布式锁。
只支持固定间隔，没有 cron、执行记录、手动触发。

## 目标

- Deferred 组件 `SchedulerComponent`：`activate` 时开始调度，关闭时收到 `CancellationToken`，在两轮之间退出；任务能拿到退出信号，长任务中途检查
- 任务启动时注册（实现 `Job` trait 或传闭包），调度方式支持固定间隔和 cron
- 命名：调度单元叫 job（`Job`、`JobId`、`job_name`），不叫 task，避免和 tokio 的 task（`tokio::spawn`、`JoinHandle`）混淆；
  日志、执行记录里也用 job
- 多实例：同一个任务的同一次触发只在一个实例上跑；实例挂了，下一次触发由别的实例接手
- 任务的开关、cron 表达式、参数可以放动态配置，改表生效，不用重启
- 执行记录：每次触发的开始、结束、耗时、结果、错误、在哪个实例上跑的，可查

## 要定的

### 多实例协调

- **方案 A：每次触发前抢锁**。每个实例都跑调度循环，到点后抢 `lock_key(任务名 + 本次触发时刻)`，抢到的执行。
  实现简单、复用现有锁；但"本次触发时刻"要各实例算得一致（cron 按同一时区、同一基准），且锁只防同时执行，
  一个实例执行完释放后，时钟慢的实例可能再抢到同一次触发 → 需要配合下面的触发状态表去重
- **方案 B：触发状态表 + 行级认领**（xxl-job 的做法）。表里存每个任务的 `next_fire_at`，实例在一个事务里
  `SELECT ... WHERE next_fire_at <= now FOR UPDATE SKIP LOCKED`，算出下次时间写回，提交后执行。
  同一次触发天然只被认领一次，不依赖各实例时钟一致（用数据库的 `now()`）
- 倾向 B（PG、MySQL 8 都有 `SKIP LOCKED`）；SQLite 单写者，直接用写事务
- 认领后实例崩溃：这次触发算丢了（at-most-once），还是记执行记录 + 超时后允许别的实例重跑（at-least-once）？要选默认值

### 调度策略（照 xxl-job 取舍）

- 阻塞处理：上一次还没跑完、下一次到点了 → 跳过（默认，和应用侧最小实现一致）/ 排队 / 覆盖。多实例时"上一次还在跑"要看执行记录，不只看本进程
- 过期策略：停机或积压错过了多次触发 → 忽略只算下一次（默认）/ 立即补一次
- 分片广播：同一次触发发给所有存活实例，各自拿到 `(index, total)`。需要知道存活实例 → 要不要做实例心跳表，还是先不做
- 失败重试、超时：放调度器里还是留给任务自己
- 手动触发、暂停 / 恢复：通过动态配置还是单独的接口

### 表

- 任务定义放哪：代码注册 + 动态配置覆盖（开关、cron、参数），还是单独一张任务表
- 触发状态表：`job_name`、`next_fire_at`、`last_fire_at`、`version`
- 执行记录表：`id`、`job_name`、`fire_at`、`started_at`、`finished_at`、`status`、`instance`、`error`；保留多久、谁来清理
- 建表规则沿用锁表、动态配置：表在且列对上就用，不在就建，对不上拒绝启动；表名前缀同 `PgLockConfig`

### cron 和时间库

- 解析用 croner 还是 `cron` crate，还是自己写：croner 3.x 必选 chrono；4.x chrono 是默认 feature，可换成 jiff；都不支持 `time`
- hygiea 对外的时间类型统一用什么，边界上怎么转
- 时区：默认 UTC（没有夏令时问题）；要支持 `Asia/Tokyo` 这类时区就要带时区库，夏令时跳过 / 重复的处理以所用库为准，写进文档
- 6 段（带秒）还是 5 段，和 xxl-job / Spring 的 cron 写法是否兼容

### 其他

- 不开数据库协调时（单实例、测试）退化成纯进程内调度，接口不变
- 任务执行时的 tracing span（任务名、触发时刻、实例），和执行记录的 id 串起来
- 指标：每个任务的执行次数、失败次数、耗时
