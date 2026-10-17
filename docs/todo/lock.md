# 分布式锁待办

现在的实现见 `docs/design/lock.md`，行为和限制见 `hygiea-db/src/lock/mod.rs`。下面是还没做、还没定的。

## 还没做的后端和实现

- Redis：`SET NX PX` + token，释放用 Lua 校验 token；要不要看门狗续期待定
- 阻塞版的默认实现是轮询（最多 100ms 一次）：释放后最多 100ms 才发现、不公平、频繁打后端。有原生等待的后端覆盖它（PG、MySQL、`TokioLock` 已覆盖）

## 方法分组和命名（已定，没做）

| 语义 | 字符串 key | i64 | i32 | 返回 |
|---|---|---|---|---|
| 不等 | `try_lock` | `try_lock_i64` | `try_lock_i32` | `Result<Option<LockGuard>>` |
| 最多等 `t` | `try_lock_timeout` | `try_lock_i64_timeout` | `try_lock_i32_timeout` | `Result<Option<LockGuard>>` |
| 一直等 | `lock` | `lock_i64` | `lock_i32` | `Result<LockGuard>` |

- 现在的 `lock(key, t)` 系列改名成 `try_lock_*_timeout`，语义不变；新加不带 `timeout` 的 `lock` 系列
- 名字是"动作 + key 类型 + 等待方式"：`try_lock_for_i64` 会被读成"给 i64 用的"，所以不用 parking_lot 的 `for`；
  `lock_wait` 也不用（现有的带超时版本本来就会等）
- 带 `try` 的可能拿不到，返回 `Option`；不带的一定拿到或出错
- `t` 只限 PG 上等锁的时间（`lock_timeout`），不是整个调用的截止时间：取池连接、建连、锁表第一次 `ensure`、BEGIN 都可能先等，
  `try_lock` 也不保证整个函数立即返回。文档写明；要做端到端截止时间得贯穿这几段，并先解决下面已知限制里的取消问题，不能只在外面套 `timeout`
- 新 `lock` 的实现：
  - PG：BEGIN 那条消息里 `lock_timeout = 0`（`sql::postgres::begin_sql(0)`），其余照旧，见调查记录（二）的结论
  - `TokioLock`：去掉外层 `tokio::time::timeout`，直接 `entry.lock_owned().await`；取消时 `Cleanup` 照常清理
  - trait 默认实现：不设截止时间，每隔最多 100ms 轮询 `try_lock`

## 错误分类（已定，没做）

core 的 `LockErr` 加 `WaitInterrupted`（`99900306`，`Lock wait interrupted: {{ key }}`）：等锁时被后端按自己的规则中断。
sqlx 的错误能把"PG 服务端中断"和"连接问题"分开：

| 情况 | `try_lock*` / `try_lock_*_timeout` | `lock*` |
|---|---|---|
| 拿到了 | `Ok(Some(guard))` | `Ok(guard)` |
| 自己 `SET LOCAL` 的超时到了（55P03） | `Ok(None)` | 不会发生（没设） |
| 库的 `lock_timeout` 非 0（55P03） | 不会发生（被 `SET LOCAL` 覆盖） | `WaitInterrupted` |
| `statement_timeout` 或 `pg_cancel_backend`（57014） | `WaitInterrupted` | `WaitInterrupted` |
| `PoolTimedOut`、`Io`、`Tls`、`PoolClosed`、其他 SQL 错误（如锁表残留行的 23505） | `AcquireFailed` | `AcquireFailed` |

## 取消等锁时服务端那半（没定）

调用方取消等待时，客户端已经把连接关掉（`close_on_drop`，container 测试 `cancelled_wait_frees_pool_connection`），
但 PG 后端等锁时不读客户端连接，照样挂在锁队列里占一个服务端连接，直到拿到锁才发现断了。两个办法：

- 再发 CancelRequest：要和关连接同步，否则迟到的取消会打断复用这条连接的下一条查询（关连接后就不存在复用问题）
- 事务里 `SET LOCAL client_connection_check_interval`：直连时起作用（经 PgBouncer 时后端的对端是 PgBouncer，不起作用）。
  实测直接杀掉客户端进程时，设 50ms 约 61ms 后端退出，设 0 时 820ms 后还在等。
  这个参数只在支持的操作系统上能设成非 0（Linux、macOS、BSD 等，PG 文档），其他平台上 `SET` 会报错、整个 BEGIN 失败，所以先没加

## PG 表结构检查改查 `pg_class` / `pg_attribute`（没做）

现在查列用 `information_schema.columns`（`sql/postgres/mod.rs`），对表没有权限的用户看到 0 列：几个用户混用一套表时
（比如锁表由另一个用户建），会以为表不存在、去建表、报已存在。`pg_class` / `pg_attribute` 不受权限影响。

## 没验证的

- AWS RDS Proxy 遇到 `SET` 会 pinning，锁事务里的 `SET LOCAL` 会不会触发（没有 AWS 实例，PgBouncer 的结果不能代替）

## 调查记录（三）：锁和动态配置用单独的库

想法：锁和动态配置不放业务库，单独一套连接配置连一个"协调库"，可以跨项目共用。几种布局：

| 布局 | 说明 |
|---|---|
| L0（现在） | 业务库、业务连接池 |
| L1 | 协调库，所有应用一个 schema，靠 `app_name` 前缀区分表（`{app}_lock`、`{app}_config` ..） |
| L2 | 协调库，每个应用一个 schema（`ALTER ROLE app IN DATABASE coord SET search_path = app`，或连接配置的 `schema_search_path`）。代码只看 `current_schema()`，不用改 |
| L3 | 每个应用一个库（同一个集群或不同集群） |

### 实测（同一个集群上建 coord 库、other 库，两个普通用户 a、b）

| 测试 | 结果 |
|---|---|
| a 在 coord 库持有 advisory 42，b（另一个用户）try | 拿不到：advisory lock 按库区分，不按用户、不按 schema |
| 同上，b 的 `search_path` 是自己的 schema | 拿不到 |
| b 在 other 库（同一个集群）try 42 | 拿到：不同库互不影响 |
| advisory lock 要什么权限 | 不要：库里任何能连上的用户都能拿任意 key |
| L1：b 往 a 的表插入 | `permission denied for table a_lock` |
| L1：b 查 a 的表 | `information_schema.columns` 0 列，`pg_class` 能看到表名 |
| L1：b 用同一个前缀建表 | `relation "a_lock" already exists` |
| L2：各自 `search_path` 后 `current_schema()` | 各是自己的 schema |
| L2：两个应用都叫 `api` | 各自 schema 里都建出 `api_lock`，不冲突 |
| L2：b 碰 a 的 schema | `permission denied for schema hy_probe_a` |
| NOTIFY：a 在 coord 库 `LISTEN api_config`，b 在 coord 库（另一个 schema）`pg_notify('api_config', ..)` | 收到：NOTIFY 按库区分，不按 schema |
| 同上，b 在 other 库发 | 收不到 |
| `max_connections` | 整个集群一个（本机 100）；可以再按库 `ALTER DATABASE .. CONNECTION LIMIT`、按用户 `ALTER ROLE .. CONNECTION LIMIT` 分 |

代码上的相关事实：

- 动态配置的 NOTIFY 频道是 `<schema>.{p}config`。收到通知只看载荷里的版本号是否比当前大，大了就重新读**自己的表**、用表里读出的版本号，
  所以串到别人的通知只是多读一次，不会读错（`live_config/worker.rs` 的 `Signal::Changed` 和 `update`）
- 建表串行化的 advisory lock 是 `(999999999, LiveConfigDdl / LockTableDdl)`，所有应用共用，只是互相排队建表，没问题
- 整数 key（`try_lock_i64(42)`、`try_lock_i32(类型, id)`）和没开 `lock-row` 的哈希字符串 key 都是 advisory lock，**不带 app_name**；
  `try_lock_i32` 的锁类型带项目前缀（`err_code_project_prefix`），不同项目用不同前缀就不撞，同一个项目的两个部署（比如 prod 和 staging 共用协调库）会撞。
  开了 `lock-row` 的字符串 key 按应用的表区分

### 各布局在共用协调库时的隔离

| | L1 一个 schema + 前缀 | L2 每应用一个 schema | L3 每应用一个库 |
|---|---|---|---|
| 表（锁表、配置表） | 靠 `app_name` 区分，`app_name` 要在所有项目、所有环境里唯一，撞了就悄悄共用锁和配置 | 按 schema 区分，`app_name` 可以重复 | 按库区分 |
| advisory lock（整数 key、哈希 key） | 整个库共用，不同应用的同一个数互斥 | 同左 | 隔离 |
| NOTIFY | 按前缀区分 | `app_name` 重复时串（只多读一次）；已改成 channel 带 schema 名，不再串 | 隔离 |
| 权限 | 每应用一个用户时表互相碰不到；一个用户就完全不隔离。advisory lock 都不隔离 | 按 schema 授权，干净 | 按库授权 |
| 表结构检查 | 每应用一个用户时 `information_schema` 看不到别人的表，名字撞了会去建表报已存在（改查 `pg_class` 能看到） | 只看自己 schema | 只看自己库 |
| 库级参数（`ALTER DATABASE coord SET ..`） | 所有应用共用 | 共用；按角色 `ALTER ROLE .. IN DATABASE` 可以分 | 每个库自己设 |
| 要 DBA 做的 | 建库、建 schema、授权 CREATE | 建库、每应用建 schema 和用户（或配置里写 `schema_search_path`，经 PgBouncer 要配 `track_extra_parameters`） | 每应用建库 |
| 跨项目共用同一把锁（有意协调） | 能（约定同一个 `app_name` 或整数 key） | 整数 key 能；字符串 key（lock-row）不能（表在各自 schema） | 不能 |

### 单独协调库相对 L0 的优劣

好处：

- 不碰业务库：业务库的 DBA 策略（超时、`search_path`、权限）不用为锁和动态配置让步；协调库可以整库按锁的需要设（`ALTER DATABASE coord SET .. = 0`），
  "要求库里是 0"在这里不影响业务查询
- 连接隔离：持有锁、动态配置的 listener 占的是协调库的连接，不和业务池、业务库的 `max_connections` 抢（不同集群时完全隔离，同一个集群只隔离池）
- 业务库不是 PG 也能用：业务用 SQLite / MySQL 的项目，锁和动态配置（PG 的 LISTEN、advisory lock）可以走 PG 协调库
- 动态配置集中：多个项目的运行时配置在一个地方看、改
- 业务库的主从切换、只读从库不影响锁和配置（动态配置现在要求开在主库连接池上）

代价：

- 多一个依赖和故障点：协调库挂了，所有共用它的项目的锁和动态配置一起失效，而业务库可能还好着；L0 下锁和业务库同生共死，故障面不变
- 爆炸半径跨项目：一个项目持锁过多、listener 过多、慢查询，影响共用协调库的其他项目
- 连接预算按所有项目的实例算：每个实例动态配置 1 条（listener，要会话级连接）+ 同时持有的锁数
- 协调库前面放 PgBouncer transaction 模式时 `LISTEN` 用不了（会话级），动态配置退化成轮询（默认 60s）；锁不受影响
- 运维多一个库：建库、账号、备份（锁表 UNLOGGED 不用备份，配置表和历史要备份）、监控
- 锁和业务数据不在一个库：现在锁事务本来就和业务事务分开，不损失能力；动态配置的修改不能和业务数据在一个事务里（一般用不到）
- 多一跳网络（协调库在别处时）

### 没定

1. 布局推荐哪个：L1 最省事但 `app_name` 要全局唯一、advisory 不隔离；L2 隔离表和权限、advisory 仍不隔离；L3 全隔离、运维最多
2. 配置里显式告诉框架前面有没有 transaction 模式的连接池（PgBouncer transaction 模式、Supavisor 等），
   比如 `[db.connect] pooler = "none" | "transaction"`（名字没定；PgBouncer session 模式和直连一样，算 `none`）。
   框架检测不出来（见调查记录二），但知道了以后可以按情况选做法：

   | 能力 | `none`：直连（或 session 模式） | `transaction`：transaction 模式的连接池 |
   |---|---|---|
   | 锁事务里的超时参数 | 两种都行：锁专用池用启动参数（每个事务不用带）；或 `begin_with` 里 `SET LOCAL` | 只能 `begin_with` 里 `SET LOCAL`（启动参数被拒或被悄悄丢掉） |
   | 锁的级别 | 可以用**会话级** advisory lock（`pg_advisory_lock` / `pg_advisory_unlock`）：持锁时不开事务，不受 `idle_in_transaction_session_timeout` / `transaction_timeout` 影响，不挡 vacuum。要处理：guard 持有连接，释放时执行 unlock 再还回池；不在运行时里 drop 时没法异步 unlock，只能把连接关掉（断开即释放）；等锁的超时还是要设。另外：持锁时连接是 idle，`idle_session_timeout` 非 0（或代理回收空闲连接）会断开并悄悄放锁，要一起设 0；同一会话重复加锁有计数，unlock 要配对；取消落在"服务端已拿到、客户端没收到响应"时没有事务回滚兜底，连接不能直接还回池，要确认 unlock 或关掉连接 | 只能**事务级**：会话级的锁拿在服务端连接上，事务结束这条连接就给了别的客户端，之后 unlock 会在另一条服务端连接上执行、失败，锁留在别人的会话里 |
   | 字符串 key 的精确锁 | lock-row（有 xid，持锁期间挡住同库 vacuum）；或者会话级 advisory + 哈希（有损） | lock-row |
   | `search_path`（框架表放在哪） | 启动参数（现状） | PgBouncer 要配 `track_extra_parameters` 带 `search_path`；或者框架的表名直接带 schema 限定，不依赖 `search_path` |
   | 动态配置的 `LISTEN` | 正常 | 用不了（会话级）：给 listener 单独配一个直连地址，或者只轮询并缩短间隔 |
   | sqlx 的预编译语句（按连接缓存命名语句） | 正常 | PgBouncer 1.21 起支持（`max_prepared_statements`），更老的要关掉 sqlx 的语句缓存（`statement_cache_capacity(0)`）。PgBouncer 1.24.1 + `max_prepared_statements = 10` 用 psycopg 3.2.12 测过：客户端切到另一条服务端连接后缓存的命名语句仍能执行；**sqlx 没测** |
   | 配置写错了 | 实际有连接池却写 `none`：启动参数被拒（启动报错，显眼）或被悄悄丢掉（启动时查 `current_setting` 能发现，可以据此拒绝启动） | 实际直连却写 `transaction`：都能用，只是做法保守 |
