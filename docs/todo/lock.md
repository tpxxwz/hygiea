# 分布式锁待办

现状见 `docs/design/lock.md`。字符串 key 的锁表（`lock-row`）已经改成事务里插入不提交的行、靠主键冲突互斥（UNLOGGED，
表一直是空的）；阻塞版 `timeout` 必须是 1ms～`i32::MAX` ms 的整数毫秒，否则 `LockErr::InvalidTimeout`。
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
- 新 `lock` 的实现：
  - PG：开事务后直接 `pg_advisory_xact_lock` / `INSERT`，**不发 `set_config`**（省一次往返），等待时间以库的配置为准，见下面的数据库要求
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

- 调用方取消等待（外层 `timeout`、`select!`）：future 被 drop、sqlx 断开连接，但 PG 后端等锁时不检查客户端连接，会一直挂在锁队列里
  占着一个服务端连接，拿到锁、回结果时才发现断了并回滚。带超时的版本最多挂 `t`，`lock` 可能一直挂。
  PG 14+ 的 `client_connection_check_interval` 能让后端定期检查连接，能不能在事务里设、效果如何没测
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
- 多人同时 try（1ms）：20 个会话同一时刻抢空闲 key，3 轮每轮恰好 1 个拿到、19 个 55P03。服务端计时 50 次：
  不冲突 0.005 / 0.006 / 0.99 ms（最小 / 中位 / 最大），冲突 1.07 / 1.20 / 2.97 ms
- `lock_timeout` 是毫秒整数参数：小数四舍五入（`'0.4ms'` 成 0 即不限时），所以最小有效值 1ms
