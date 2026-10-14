# 分布式锁待办

现状见 `docs/design/lock.md`。字符串 key 的锁表（`lock-row`）已经改成事务里插入不提交的行、靠主键冲突互斥（UNLOGGED，
没有已提交的行）；阻塞版 `timeout` 必须是 1ms～`i32::MAX` ms 的整数毫秒，否则 `LockErr::InvalidTimeout`。
下面是讨论过、还没定或还没做的。

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

## 数据库超时参数（没定）

实测（本机 PG 18.1）：`lock_timeout` 和 `statement_timeout` 都会打断等锁，advisory lock 和 INSERT 一样，哪个先到算哪个。
另外两个影响的是**持有**：拿到锁后业务在 Rust 里跑时，锁事务是 idle in transaction。

| 参数 | 影响 | 内置默认 | 生产上 |
|---|---|---|---|
| `lock_timeout` | 等锁 | 0 | 较少全局设，多在迁移 / DDL 时临时设 |
| `statement_timeout` | 等锁（整条语句的时间） | 0 | 常设，防慢查询；常按角色设 |
| `idle_in_transaction_session_timeout` | 持有：超时 PG 断开连接，锁被悄悄释放，业务不知道 | 0 | 常设，防忘提交的事务；云服务的参数组可能有非 0 默认值 |
| `transaction_timeout`（PG 17+） | 等锁 + 持有 | 0 | 很新，用得少 |

`lock` 一直等，所以 `statement_timeout` 非 0 迟早会打断它，不是"比最长等锁时间长就行"。
持有被悄悄释放影响所有方法（包括 `try_*`），比等锁被打断更危险。

### 启动时校验

在 `prepare`（`connect_lazy` 时第一次加锁）里 `SELECT current_setting(..)` 查这四个的实际值（服务端、`ALTER DATABASE`、`ALTER ROLE`
叠加后的结果，同一个池用同一个库和用户，查一条连接就够）。查不到的是业务代码运行时在池里的连接上做的会话级 `SET`，
这个只能靠运行时的 `WaitInterrupted`。

没定：不是 0 时拒绝启动，还是只打警告（只用 `try_*` 的项目也会被拦住）。

### 怎么让这几个参数是 0，又不让业务查询失去保护

| 做法 | 每次加锁多发 SQL | 要 DBA | 业务查询的保护 |
|---|---|---|---|
| 要求库 / 角色上就是 0 | 不发 | 要（`ALTER ROLE app SET ... = 0`） | 没了 |
| 每次加锁 `SET LOCAL` | 发 | 不要 | 保留 |
| 单独的锁连接池，建连时带启动参数（sqlx `PgConnectOptions::options`） | 不发 | 不要（这几个参数普通用户就能设，启动参数优先于 `ALTER ROLE`） | 保留 |
| 单独的锁连接池 + 单独的用户（DBA 给锁用户 `ALTER ROLE lock_user SET ... = 0`） | 不发 | 要，多一套账号 | 保留 |

单独的锁连接池另外的好处：持有锁占连接，现在用的是业务池，锁多了业务拿不到连接，反过来业务池满了加锁也卡住；分开后互不影响。
锁的事务本来就和业务事务分开（guard 持有自己的事务），分开池子不丢能力。

- PgBouncer（连接池代理，生产上多用 transaction 模式，事务结束就把真实连接还回去）的 transaction 模式下启动参数通常被忽略或拒绝
  （`ignore_startup_parameters`），只能用单独的用户或者 `SET LOCAL`。锁本身不受影响：事务级的锁在事务期间占着同一条真实连接
- 单独的用户：advisory lock 和锁表主键冲突都按库区分、不按用户，不同用户之间照样互斥。默认 `search_path` 是 `"$user", public`，
  有和锁用户同名的 schema 时锁表建在那里，所有实例用同一个锁用户就一致

倾向（没定）：单独的锁连接池，默认用启动参数把四个超时设成 0，配置里可以另填账号密码（就成了单独的用户）；启动时在锁池上校验，
顺带能发现启动参数被 PgBouncer 吃掉的情况。

没定：

1. 锁池挂在现有连接池配置下面（`lock` 子配置，内部再建一个 sqlx 池），还是独立组件（如 `PgLockPool`，单独放进 Resources）
2. 锁池不配时：退回主池（现在的行为），还是开了 `lock` feature 就必须配

## 已知限制（要写进文档）

- 调用方取消等待（外层 `timeout`、`select!`、任务 abort）：sqlx **不断开连接，也不发 CancelRequest**。`Transaction::drop` 把 ROLLBACK
  排进发送队列，`PoolConnection::drop` 在后台归还，归还时 `ping()` 要等原来那条加锁语句的响应。所以等锁的语句在 PG 上照常排队，
  客户端池里这条连接也一直占着，直到拿到锁、回滚完才回到池里。带超时的版本最多占 `t`，`lock` 可能一直占。
  实测（sqlx 0.9.0）：`max_connections = 1` 的池等别人持有的 advisory lock，100ms 后丢掉 future；再过 200ms 后端仍是 `active / Lock / advisory`，
  池 `size = 1, num_idle = 0`，再 `acquire` 300ms 报 `PoolTimedOut`；持有者释放后才恢复。
  `WaitInterrupted` 解决不了（没人接收结果）。
  **已改（客户端这半）**：锁不再用 sqlx 的 `Transaction`，自己取连接（`LockTx`），没走到 ROLLBACK 就被丢掉时 `close_on_drop` 关掉连接，
  池里马上空出位置（container 测试 `cancelled_wait_frees_pool_connection`，本机没 Docker 还没跑）。
  服务端那半没解决：PG 后端等锁时不读客户端连接，照样挂在锁队列里占一个服务端连接，直到拿到锁才发现断了。
  没定：要不要再发 CancelRequest（要和关连接同步，否则迟到的取消会打断复用这条连接的下一条查询；关连接后就不存在复用问题），
  或者事务里 `SET LOCAL client_connection_check_interval`：现在客户端真的关了 TCP，它就能起作用了（直连时；经 PgBouncer 时后端的对端是
  PgBouncer，不起作用）。实测直接杀掉客户端进程时，设 50ms 约 61ms 后端退出，设 0 时 820ms 后还在等。
  这个参数只在支持的操作系统上能设成非 0（Linux、macOS、BSD 等，PG 文档），其他平台上 `SET` 会报错、整个 BEGIN 失败，所以先没加
- 连接预算要算上等锁的：每把 PG 锁各开一个事务，持有的和正在等的都占连接。拿着锁再用同一个池做业务查询，池小时会等自己占着的连接；
  两个任务分别持有 A、B 再互相等 B、A 时，持有的事务在 PG 上是 idle、等的在另一条连接上，PG 的死锁检测看不到 Rust 任务之间的依赖。
  带超时的会报错退出，`lock` 会永远卡住。不可重入（自己持有再阻塞拿同一个 key 也是自等）。文档写明：多把锁约定全局获取顺序、按等待者算连接预算；
  单独锁池只能防止业务池被锁占满，锁池自己照样会耗尽、照样有顺序环（结构分析，没复现）
- 连接失效时锁会放掉，但持有 `LockGuard` 的业务不会停：四个超时设 0 只防这几个参数主动杀事务，挡不住 `pg_terminate_backend`、网络断开、重启、
  主库切换。这时另一个实例能拿到锁，两边同时执行；`release()` 报错只能说明释放没确认，撤销不了之前的副作用，加"是否还持有"的查询也有检查之后的窗口。
  框架只保证连接正常时互斥；不可重复的写入要靠业务幂等、条件更新或 fencing token（现在的接口不提供 token）
- lock-row 的表没有已提交行，但回滚留下死元组和死索引项，文件照样长，靠 vacuum 回收（UNLOGGED 只省 WAL，不省 vacuum）。
  实测关掉 autovacuum 回滚 2000 次：0 行、204800 字节，VACUUM 后 122880 字节（普通 VACUUM 只把空间变成可复用，不保证缩回）。
  回收跟不上（长持锁卡住清理边界、高频加锁）时会膨胀，见调查记录（四）
- `try_lock`（lock-row）没拿到时 PG 服务端日志记一条 `ERROR: canceling statement due to lock timeout`，
  默认 `log_min_error_statement` 还带上语句；这两个日志参数要超级用户才能改
- 不保证先来先得：多个人在等时谁先拿到不确定（PG、哈希降级都是；`TokioLock` 是先到先得）

## 调查记录

本机 PG 18.1，临时 schema 测完删掉。

- 对方插入未提交时：普通 `INSERT`、`ON CONFLICT DO NOTHING` 都等到对方结束（对方回滚后插入成功）；`lock_timeout = 0` 也一直等；
  `lock_timeout = '1ms'` 立即报 55P03，CONTEXT 是 `while inserting index tuple .. in relation "t_pkey"`。
  对方已提交时 `INSERT` 立即报 23505，`ON CONFLICT DO NOTHING` 立即返回。PG 文档 Index Uniqueness Checks：
  "If a conflicting row has been inserted by an as-yet-uncommitted transaction, the would-be inserter must wait"；
  `_bt_check_unique` 里 `XactLockTableWait`，没有不等的分支；`DEFERRABLE` 唯一约束插入时不等但提交时才检查，锁永远回滚，等于不互斥
- 多人排队：A 持有 2s，B、C、D 同时等，依次在 2.06s、4.08s、6.08s 拿到，同一时刻只有一个
- 多人同时 try（1ms）：20 个会话同一时刻抢空闲 key，3 轮每轮恰好 1 个拿到、19 个 55P03。
  客户端看到的耗时 4～27ms（拿到的那个也要 3～21ms），是本机同时唤醒 20 个 psql + 20 个后端进程的调度噪声，不是等锁；
  所以另在服务端计时 50 次（一个连接里循环，每次 INSERT 放在 PL/pgSQL 的 EXCEPTION 子事务里，等锁机制和顶层事务一样）：
  不冲突 0.005 / 0.006 / 0.99 ms（最小 / 中位 / 最大），冲突 1.07 / 1.20 / 2.97 ms
- `lock_timeout` 是毫秒整数参数：小数四舍五入（`'0.4ms'` 成 0 即不限时），所以最小有效值 1ms
- 两种锁表写法每次加锁写多少 WAL（分析，没实测）：
  - 行先提交、再 `FOR UPDATE`：每次加锁一条行锁记录（只改元组头），不留死元组；行数 = 用过的不同 key 数，按订单号这类 key 加锁时一直涨
  - 事务内 INSERT、回滚撤销（现在的写法）：普通表每次加锁写堆元组插入 + 主键索引插入 + abort 记录，留一个死元组和死索引项给 vacuum；
    表不增长。建成 UNLOGGED 后堆和索引的插入不写 WAL，只剩事务的 abort 记录
  - 选现在的写法是为了已提交的行数不随 key 的个数增长，不是为了写得少；物理文件仍会因死元组增长，见已知限制

## 调查记录（二）：怎么让锁事务里的超时参数是 0

上面"数据库超时参数"一节的倾向（单独锁池 + 启动参数）是这轮调查之前写的，保留作对照；这轮的结论见本节最后。
环境：本机 PG 18.1（容器），PgBouncer 1.24.1（`edoburu/pgbouncer:v1.24.1-p1`，transaction 模式，`default_pool_size = 1`）。
临时角色、schema、库、容器测完都删了。

### 启动参数（sqlx `PgConnectOptions::options`）

sqlx 0.9 把 `options([(k, v)])` 拼成 `-c k=v` 放进启动包的 `options` 参数，和 libpq 的 `PGOPTIONS` 同一个机制，用 psql + `PGOPTIONS` 模拟。

| 测试（直连 PG） | 结果 |
|---|---|
| 角色上 `ALTER ROLE app SET` lock 3s / statement 5s / idle_in_tx 10s，直连 | 3s / 5s / 10s |
| 同一用户，启动参数三个都设 0 | 0 / 0 / 0：启动参数优先于 `ALTER ROLE` |
| DBA 能不能禁止用户覆盖 | 不能：四个参数的 `pg_settings.context` 都是 `user`，用户会话里 `SET` 也能改 |
| 带启动参数的连接跑 `pg_sleep(6)`（角色 statement 5s） | 不被打断：这条连接上所有语句都没了保护，只适合锁专用的池 |

### 单独的用户（同库同 schema）

| 测试 | 结果 |
|---|---|
| 跨用户 advisory lock、锁表 INSERT 主键冲突 | 都互斥（锁表要有 INSERT 权限） |
| 锁用户默认 `search_path` | `"$user", public` → 落到 `public` |
| 锁用户在 `public` 建表 | `permission denied for schema public`（PG 15 起 public 不给普通用户 CREATE） |
| 在业务 schema 建表 | 要 `GRANT USAGE, CREATE ON SCHEMA` |
| 有和锁用户同名的 schema | 表建到那个 schema |
| 业务用户对锁表没有权限时查 `information_schema.columns` | 0 列：现在的表结构检查用它，混用两个用户的实例会以为表不存在、去建表、报已存在。改成查 `pg_class` / `pg_attribute` 不受权限影响 |

### 经过 PgBouncer（transaction 模式）

| 测试 | 结果 |
|---|---|
| 带启动参数 `-c statement_timeout=0`，默认配置 | 拒绝连接：`unsupported startup parameter in options: statement_timeout` |
| `ignore_startup_parameters = options` | 连得上，设置被悄悄丢掉（5s） |
| `track_extra_parameters` 加上三个超时 | 连得上，设置**也没生效**（3s / 5s / 10s），等锁 3s 被角色的 `lock_timeout` 打断。推测 PgBouncer 只能跟踪 PG 主动上报（ParameterStatus）的参数，没核实 |
| 事务里 `SET LOCAL` | 生效 |
| 会话级 `SET statement_timeout = '77s'` 后断开 | 下一个客户端（同一条服务端连接）看到 77s：泄漏 |
| 单独的锁用户，`ALTER ROLE` 设成 0，经 PgBouncer | 0 / 0 / 0，生效 |
| 经 PgBouncer 的 advisory lock 和直连的 | 互斥 |
| 启动参数 `-c search_path=..`，默认配置 | 拒绝连接：`unsupported startup parameter in options: search_path` |
| `track_extra_parameters = IntervalStyle, search_path` 后带 `search_path` | 生效，另一个不带的客户端是默认值，没泄漏（只测了 PG 18） |

注意：现有的 `schema_search_path` 配置就是用启动参数传的（`sqlx/pool/postgres.rs` 的 `search_path_goes_into_options`），
经 PgBouncer transaction 模式时要 PgBouncer 配 `track_extra_parameters` 带上 `search_path`，否则连不上。要写进文档。

能不能检测出中间有 PgBouncer：不能可靠检测。它不在协议里表明身份，转发真实 PG 的参数；比端口（6432 vs `inet_server_port()`）
会被 k8s Service / NAT 误判，比两个事务的 `pg_backend_pid()` 负载低时常常不变；带启动参数连只能发现"报错"，发现不了"悄悄丢掉"。

### 事务里 `SET LOCAL`，和 BEGIN 合成一次往返

sqlx 0.9 的 `pool.begin_with(sql)` 用简单查询协议发自定义 BEGIN（一条消息可以有多条语句），发完检查连接确实在事务里，
不在报 `BeginFailed`。所以 `BEGIN; SET LOCAL ..; SET LOCAL ..` 是一次往返。

| 测试 | 结果 |
|---|---|
| 会话 `idle_in_transaction_session_timeout` 2s，事务里 `SET LOCAL` 0，空闲 3s | 连接还在；对照组（不 SET LOCAL）被断开 `terminating connection due to idle-in-transaction timeout` |
| 会话 `transaction_timeout` 2s，事务里 `SET LOCAL` 0，事务 3s | 还在；对照组被断开 `terminating connection due to transaction timeout` |
| 一条消息 `BEGIN` + 四个 `SET LOCAL .. = 0` | 四个都是 0，回滚后恢复原值 |
| 经 PgBouncer：角色上 lock 3s / statement 5s / idle 2s / transaction 4s，一条消息 BEGIN + 四个 SET LOCAL | 0 / 0 / 0 / 0；之后空闲 3s 再跑 6s 的语句，事务还在；下一个客户端（同一条服务端连接）看到的是角色原值，没泄漏 |

结论（**已实现**，`hygiea-db/src/sql/postgres/mod.rs` 的 `begin_sql`；锁和两个建表事务都用，另加了 `ISOLATION LEVEL READ COMMITTED`）：锁的事务一律 `begin_with("BEGIN; SET LOCAL lock_timeout = ..; SET LOCAL statement_timeout = 0; SET LOCAL idle_in_transaction_session_timeout = 0; [SET LOCAL transaction_timeout = 0]")`：

- 直连、PgBouncer 都是同一套，不用检测；不用单独锁池、不用单独用户、不用 DBA 改配置、不用启动校验
- 往返不增加：`try_lock_*_timeout` 从三次（BEGIN、set_config、加锁）变两次；`lock` 两次，和"不发 SET"一样
- 只在锁事务里是 0，业务查询保留 DBA 的保护；持有期间被 `idle_in_transaction_session_timeout` 悄悄释放的问题也一起解决
- 值是框架自己拼的整数，拼进 SQL 安全（`AssertSqlSafe`）
- 只支持最新的 PG，`transaction_timeout` 直接带上，不查版本、不做低版本兼容
- **中途出错要处理连接**：BEGIN 成功、后面的 `SET LOCAL` 报错时，sqlx 0.9.0 的自定义 BEGIN 要全部响应成功才加 `transaction_depth`，
  回滚保护 `start_rollback` 只在 depth > 0 时入队 ROLLBACK，于是这条连接带着失败的事务回到池里。实测单连接池
  `begin_with("BEGIN; SET LOCAL hygiea_nonexistent_parameter = 0")` 报未知参数后，下一条 `SELECT 1` 得到 `25P02: current transaction is aborted`。
  采用这个方案时，`begin_with` 出错要显式回滚或关掉这条连接，并补回归测试。
  网络错误、取消是否同样留下 25P02 没测
- 没验证：AWS RDS Proxy 遇到 `SET` 会 pinning，`SET LOCAL` 会不会触发（没有 AWS 实例，PgBouncer 的结果不能代替）
- 推翻了上面的"`lock` 不发 `set_config`、靠库里是 0"和"启动时校验四个参数"；单独锁池只剩"持有占连接、别和业务抢"这一个理由。
  经 PgBouncer 时客户端分两个池，在 PgBouncer 那边仍是同一个用户 + 库的池，要服务端也隔离只能另建用户

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

- 动态配置的 NOTIFY 频道是 `{p}config`（带前缀）（已改：现在是 `<schema>.{p}config`，见下面"已实现"）。收到通知只看载荷里的版本号是否比当前大，大了就重新读**自己的表**、用表里读出的版本号，
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

1. 要不要支持单独的协调库；支持的话配置怎么写：比如单独一段连接配置（`[coord]`）给锁和动态配置用，不配退回业务池（L0）
   - **已实现**（不用 `[coord]`）：锁和动态配置各自一段 `connect`（`[db.lock.connect]`、`[db.live_config.connect]`，类型
     `PgConnectOverride`），每项可选，逐项盖到 `[db.connect]` 上，什么都不写就是 L0。动态配置本来就是独立小池（1 条，参数固定）；
     锁写了 `connect` 或 `max_connections` 就单独建池（只开放 `max_connections`，其余固定）
   - 表名前缀各段自己配：`table_prefix`，不写用 `app_name`，`""` 不加前缀，`foo` / `foo_` 都是 `foo_`。L1 下 `app_name` 撞了可以用它分开，
     有意共用一套表就配成一样
   - 动态配置的 NOTIFY channel 改成 `<schema>.{p}config`（建表时写进函数，超过 63 字节拒绝启动；已有函数的 channel 对不上也拒绝启动）
   - advisory lock 的整数 key 仍是库级共享，按第 4 条处理，不改
2. 布局推荐哪个：L1 最省事但 `app_name` 要全局唯一、advisory 不隔离；L2 隔离表和权限、advisory 仍不隔离；L3 全隔离、运维最多
3. 共用协调库时整数 key 不隔离怎么办：文档要求不同部署用不同的 `err_code_project_prefix` / 约定整数 key 的范围；
   或者共用协调库时整数 key 也转成字符串 key 走锁表（按应用的表区分），放弃原生 advisory
4. 补充（对第 3 条的讨论结果）：所有应用都按 hygiea 的编号规则（项目前缀 3 位 + 模块前缀 + code）注册锁类型和字符串 key，
   只要各项目的 `err_code_project_prefix` 不重复（跨项目没有工具检查，自己统一分配），不同项目之间基本不冲突：

   | key | 实际的锁 | 不同项目（前缀不同） | 同一个项目部署多份（如 prod / staging 共用协调库） |
   |---|---|---|---|
   | `try_lock_i32(锁类型, id)` | `pg_advisory(锁类型, id)`，锁类型是 9 位编号 | 不冲突 | 冲突（锁类型一样） |
   | `lock_key!` 字符串 key，开 `lock-row` | 各应用的 `{app}_lock` 表 | 不冲突 | `app_name` 不同就不冲突（同一个 schema 时） |
   | `lock_key!` 字符串 key，不开 `lock-row` | `"9 位编号:模板"` 哈希成 i64 走 advisory | 只剩哈希碰撞（100 万个 key 约 3×10⁻⁸） | 冲突（字符串一样） |
   | `try_lock_i64(n)` | `pg_advisory(n)`，裸数字 | 冲突 | 冲突 |
   | 内置 `(999999999, LiveConfigDdl / LockTableDdl)` | 建表串行化 | 有意共用，只是排队建表 | 同左 |

   - `try_lock_i64` 设计上就是裸的库级全局 key（不注册、没前缀），框架不替它分命名空间；L0 下它也和同库所有 advisory 用法共享，
     协调库只是把共享范围扩大到所有共用它的项目。不处理，文档写明：要跨项目隔离用 `try_lock_i32(注册的锁类型, id)` 或 `lock_key!`
   - 同一个项目部署多份：schema 挡不住（advisory 按库隔离），要么各用各的协调库，要么不同 `app_name` + 开 `lock-row` 且不用整数 key
5. 补充：配置里显式告诉框架前面有没有 transaction 模式的连接池（PgBouncer transaction 模式、Supavisor 等），
   比如 `[db.connect] pooler = "none" | "transaction"`（名字没定；PgBouncer session 模式和直连一样，算 `none`）。
   框架检测不出来（见调查记录二），但知道了以后可以按情况选做法：

   | 能力 | `none`：直连（或 session 模式） | `transaction`：transaction 模式的连接池 |
   |---|---|---|
   | 锁事务里的超时参数 | 两种都行：锁专用池用启动参数（每个事务不用带）；或 `begin_with` 里 `SET LOCAL` | 只能 `begin_with` 里 `SET LOCAL`（启动参数被拒或被悄悄丢掉） |
   | 锁的级别 | 可以用**会话级** advisory lock（`pg_advisory_lock` / `pg_advisory_unlock`）：持锁时不开事务，不受 `idle_in_transaction_session_timeout` / `transaction_timeout` 影响，不挡 vacuum（见调查记录四）。要处理：guard 持有连接，释放时执行 unlock 再还回池；不在运行时里 drop 时没法异步 unlock，只能把连接关掉（断开即释放）；等锁的超时还是要设。另外：持锁时连接是 idle，`idle_session_timeout` 非 0（或代理回收空闲连接）会断开并悄悄放锁，要一起设 0；同一会话重复加锁有计数，unlock 要配对；取消落在"服务端已拿到、客户端没收到响应"时没有事务回滚兜底，连接不能直接还回池，要确认 unlock 或关掉连接 | 只能**事务级**：会话级的锁拿在服务端连接上，事务结束这条连接就给了别的客户端，之后 unlock 会在另一条服务端连接上执行、失败，锁留在别人的会话里 |
   | 字符串 key 的精确锁 | lock-row（有 xid，持锁期间挡住同库 vacuum）；或者会话级 advisory + 哈希（有损） | lock-row |
   | `search_path`（框架表放在哪） | 启动参数（现状） | PgBouncer 要配 `track_extra_parameters` 带 `search_path`；或者框架的表名直接带 schema 限定，不依赖 `search_path` |
   | 动态配置的 `LISTEN` | 正常 | 用不了（会话级）：给 listener 单独配一个直连地址，或者只轮询并缩短间隔 |
   | sqlx 的预编译语句（按连接缓存命名语句） | 正常 | PgBouncer 1.21 起支持（`max_prepared_statements`），更老的要关掉 sqlx 的语句缓存（`statement_cache_capacity(0)`）。PgBouncer 1.24.1 + `max_prepared_statements = 10` 用 psycopg 3.2.12 测过：客户端切到另一条服务端连接后缓存的命名语句仍能执行；**sqlx 没测** |
   | 配置写错了 | 实际有连接池却写 `none`：启动参数被拒（启动报错，显眼）或被悄悄丢掉（启动时查 `current_setting` 能发现，可以据此拒绝启动） | 实际直连却写 `transaction`：都能用，只是做法保守 |

## 调查记录（四）：持锁的事务挡不挡 vacuum

本机 PG 18.1。业务表关掉 autovacuum，插 1000 行再删掉，手动 `VACUUM (VERBOSE)` 看死元组清不清得掉；
持锁的会话执行完加锁语句后在客户端 sleep，服务端是 idle / idle in transaction（第一次测试时持锁会话在跑 `pg_sleep`，
执行中的语句本身带快照，四种都挡，测不出差别，作废）。

| 持锁方式 | 持锁会话 | 同库业务表的 VACUUM |
|---|---|---|
| 没人持锁 | — | 1000 removed |
| 事务级 advisory（`BEGIN; SELECT pg_advisory_xact_lock(1)`，READ COMMITTED） | idle in transaction，没有 xid，没有 xmin | 1000 removed |
| lock-row（`BEGIN; INSERT .. ` 不提交） | idle in transaction，**有 xid** | **0 removed，1000 are dead but not yet removable** |
| lock-row 放在另一个库（同一个集群） | idle in transaction，有 xid | 1000 removed |
| 会话级 advisory（`SELECT pg_advisory_lock(1)`，不开事务） | idle，没有 xid | 1000 removed |
| 已提交行上 `SELECT .. FOR UPDATE` | idle in transaction，有 xid | 0 removed |
| REPEATABLE READ 的事务级 advisory | idle in transaction，没有 xid，**有 xmin** | 0 removed |

- lock-row 持锁期间事务分到了 xid，整个库的 vacuum 清理边界被它卡住：持锁越久，这个库里**所有表**的死元组越清不掉、越膨胀。
  advisory lock 不分配 xid，READ COMMITTED 下语句结束快照就释放了，不挡
- 原来"行先提交、再 `FOR UPDATE`"的写法同样有这个问题（实测，见表）：行锁要在元组上记锁持有者的 xid
- advisory 不挡是有前提的：锁事务用 READ COMMITTED、只用来加锁。REPEATABLE READ / SERIALIZABLE 的快照留着 xmin，事务里做别的操作分到 xid，都会挡。
  实现时锁事务固定隔离级别（不跟着库的 `default_transaction_isolation` 走）
- vacuum 的清理边界只看同一个库里的事务（普通表；`ComputeXidHorizons` 对共享系统表另算一个全集群的边界），所以锁表放在单独的库（协调库）就不影响业务库，
  见调查记录三。但协调库内部照样受影响：长持锁的 lock-row 会挡住同库的锁表、配置表、历史表的清理
- 对设计的影响：业务库上用 lock-row 时，持锁时间要短，或者文档写明这个代价；持锁久的场景，用单独的协调库，或者用 advisory lock（整数 key、哈希降级）
