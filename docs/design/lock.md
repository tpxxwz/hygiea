# 分布式锁：trait、key 的注册、PG 实现、进程内的锁

trait 和 key 在 hygiea-core 的 `lock` 模块（feature `lock`），后端在组件里实现：目前只有 hygiea-db 的
PostgreSQL 连接池（feature `lock`、`lock-row`）。Redis 还没有实现。
不需要跨进程时用 core 的 `TokioLock`（feature `lock-tokio`），见第 4 节。

---

## 1. trait

```rust
#[async_trait]
pub trait DistributedLock: Send + Sync {
    // 后端必须实现
    async fn try_lock(&self, key: LockKey) -> Result<Option<LockGuard>>;
    // 默认：转成字符串 key 999999999:<数字> 调 try_lock
    async fn try_lock_i64(&self, key: impl Into<LockKeyI64> + Send) -> Result<Option<LockGuard>>;
    // 默认：高 32 位锁类型、低 32 位 key2 合成 i64 调 try_lock_i64
    async fn try_lock_i32(&self, key1: impl Into<LockType> + Send, key2: impl Into<LockKeyI32> + Send) -> Result<Option<LockGuard>>;
    // 默认：轮询 try_lock（最多 100ms 一次）
    async fn lock(&self, key: LockKey, timeout: Duration) -> Result<Option<LockGuard>>;
    // 默认：和上面的 try_lock_i64 / try_lock_i32 一样一层调一层
    async fn lock_i64(&self, key: impl Into<LockKeyI64> + Send, timeout: Duration) -> Result<Option<LockGuard>>;
    async fn lock_i32(&self, key1: impl Into<LockType> + Send, key2: impl Into<LockKeyI32> + Send, timeout: Duration) -> Result<Option<LockGuard>>;
}
```

- 拿不到返回 `Ok(None)`，后端出错才是 `Err`。锁不可重入：自己持有的 key 再拿也拿不到
- 阻塞版最多等 `timeout`，超时 `Ok(None)`；`timeout` 为 0 等于 `try_*`
- `LockGuard`：`release().await` 释放并拿到结果；没调就 drop 的话 spawn 到后台释放，失败只打 WARN
- 每个方法都能覆盖。后端不想提供的方法照常实现，返回 `LockErr::Unsupported`
- 方法带 `impl Into<..>` 参数，不能 `dyn DistributedLock`，持有具体类型或者用泛型

## 2. key

| 方法 | key | 要不要注册 |
|---|---|---|
| `try_lock` / `lock` | `LockKey`：`lock_key!` 生成，`<9 位编号>:<模板渲染结果>` | `#[derive(hy_lock_key)]` |
| `try_lock_i64` / `lock_i64` | `LockKeyI64`：标准库能 `Into<i64>` 的类型，加上 `u64`（按位转） | 不用 |
| `try_lock_i32` / `lock_i32` | `LockType`（锁类型）+ `LockKeyI32`（运行时的 id，标准库能 `Into<i32>` 的类型加上 `u32`） | 锁类型要 `#[derive(hy_lock_type)]` |

`LockType`、`LockKey` 的字段私有，只能由宏生成，随手写的数字、字符串编译不过。

`LockKey::hashed_i64()` 是使用方显式选的降级：把字符串 key 哈希成 i64 走 `*_i64`（PG 上是 advisory lock，不用锁表）。
算法是 `hygiea::string::xxh3_64`（xxh3-64，seed 0），跨进程、跨版本一致；有损：n 个 key 里至少一对碰撞的概率约 n² / 2⁶⁵（100 万个约 3 × 10⁻⁸），
撞上的两个 key 互相多锁，不会少锁；和直接传数字的 `try_lock_i64` 共用同一个空间。不做 `Into<LockKeyI64>`，
免得 `try_lock_i64(lock_key!(..))` 悄悄哈希。

### 编号

规则和错误码一样，代码也共用（hygiea-macros 的 `prefix.rs`、`template.rs`）：

- 9 位 = 项目前缀 3 位（Cargo.toml 的 `err_code_project_prefix`，和错误码共用）+ 模块前缀 + 编号
  - `#[lock_module_prefix = "02"]`：2 位，编号 4 位，给使用 hygiea 的项目
  - `#[lock_internal_module_prefix = "003"]`：3 位，编号 3 位，只给 hygiea 自己（项目前缀 999）
  - 都不写：编号 6 位
- `hy_lock_type` 和 `hy_lock_key` 共用一套编号、一个注册表（`LOCK_REGISTRATIONS`），和错误码是两套
- 查重：同一个 enum 里宏报错；同模块靠常量 `HYGIEA_LOCK_<9 位>` 重名（E0428）；跨 crate 靠导出符号
  `__hygiea_lock_<9 位>`（Linux 链接期），都没拦住的由 core 的 ctor 启动时查重，打一行报错后退出
- `999999999` 是 hygiea 内置保留的（`hygiea_core::builtin::LOCK_TYPE` / `LOCK_CODE`，常开、文档里隐藏），注册表里手动占着一条，
  业务注册不到。用在两处，互不影响：`try_lock_i64` 默认实现转成的字符串 key 的前缀（`999999999:<数字>`）；hygiea 组件自己用的锁
  `builtin::Lock`，比如 hygiea-db 建表时串行化的 `pg_advisory_xact_lock(999999999, Lock::LiveConfigDdl / LockTableDdl)`
  （`(int, int)` 空间，和业务 `try_lock_i64` 的 bigint 空间分开，业务的锁类型又注册不到 999 段，所以不会撞）。
  `builtin` 不跟 `lock` feature，只开 `db-live-config` 不会带上锁

### 字符串 key 的模板

和 `err_tpl` / `err!` 一样（`lock_key!` 和 `err!` 共用 `__hygiea_tpl_args!`）：

```rust
lock_key!(K::Fixed)                          // 模板没有变量
lock_key!(K::One, user_id)                   // 正好一个变量
lock_key!(K::Two, { "shop": s, "day": d })   // 多个变量
```

模板语法、写法和变量个数、`{ .. }` 里的字面量 key 在编译期检查；key 用了表达式时查不了，运行时缺变量
`LockKey::as_str()` 返回 `LockErr::InvalidKey`。

最终的 key 带着编号，不同模板渲染出来不会撞；同一个模板有多个变量时，值里带分隔符会有歧义
（`{{ a }}:{{ b }}` 下 `"x:y"` + `"z"` 和 `"x"` + `"y:z"` 一样），只能靠使用方别这么传。

### 拦不住的

- `lock::__private::registered` / `lock_key` 是 `pub`（宏在调用方 crate 里展开要用），直接调能造出没注册过的 key，
  和 `HyErr::new` 一样只靠"不用 `__private`"的约定
- 绕过 hygiea 直接操作库（手写 `pg_advisory_lock`、改锁表）

## 3. PostgreSQL 实现（hygiea-db）

`SqlxPgPool`、`SeaOrmPgPool`（用底层的 sqlx 池）直接实现 `DistributedLock`，实现只有一份（`lock/sqlx/postgres.rs` 的 `PgLock`，
`impl_pg_lock!` 给两个连接池生成 impl）。

### 配置：`[db.lock]`（`LockConfig`）

```toml
[db.lock]
# table_prefix = "foo"     # 锁表前缀：不写用应用名；"foo" 或 "foo_" 都是 foo_lock；"" 时表名是 lock。没开 lock-row 时不用
# max_connections = 20     # 锁用的池最多几条连接，不写沿用 [db.pool] 的
[db.lock.connect]          # 单独连的库：只写和 [db.connect] 不一样的项，不写的沿用
# database = "coord"
```

- `connect`、`max_connections` 都不写：锁用连接池本身（下文的"主池"），同时持有的锁加上业务查询要在连接池的 `max_connections` 以内
- 写了任意一项：单独建一个锁用的池，不和业务抢连接。连接参数是 `connect` 逐项盖到 `[db.connect]` 上，
  `application_name` 是 `<应用名>-lock`；池参数只有 `max_connections` 能配，其余固定（不预建连接、空闲 10 分钟回收、
  最长存活 30 分钟、拿之前先检查，acquire 超时跟连接池走）。空闲回收、到期替换只作用于还回池里的连接，不会断开持锁的那条。
  连接池是 `connect_lazy` 时它也不连，否则启动时连上确认能用；连接池关闭时一起关
- 用法：锁放到单独的协调库 / schema / 用户（只有锁表权限），或者只是给锁单独的连接数

```text
try_lock_i64(k)  ── 锁的池取连接 ──> BEGIN; SET LOCAL ..(lock_timeout = 0) ──> SELECT pg_try_advisory_xact_lock(k)
                                    ├─ true  ──> guard 持有事务；release / drop 时 ROLLBACK，锁随事务释放
                                    └─ false ──> 当场 ROLLBACK，Ok(None)
lock_i64(k, t)   ── 校验 t ──> BEGIN; SET LOCAL ..(lock_timeout = t) ──> SELECT pg_advisory_xact_lock(k)
                                    └─ 55P03（lock_not_available）──> Ok(None)
```

- 锁的事务一律用一条消息开（`sql/postgres/mod.rs` 的 `begin_sql`，一次往返）：`BEGIN ISOLATION LEVEL READ COMMITTED` 加四个 `SET LOCAL`，
  `lock_timeout` 按方法设，`statement_timeout`、`idle_in_transaction_session_timeout`、`transaction_timeout` 设 0。
  只在锁事务里生效，DBA 在库、用户上设的超时既打断不了等锁、也断不开持锁，业务查询照旧受保护；经 PgBouncer transaction 模式也一样。
  READ COMMITTED 让 advisory lock 不挡同库的 vacuum。只支持最新的 PG（`transaction_timeout` 是 PG 17 加的）
- 不用 sqlx 的 `Transaction`，自己取连接、发 BEGIN / ROLLBACK：BEGIN 那条消息中途报错、加锁出错、等锁时被取消、回滚失败，
  连接都直接关掉不还回池（sqlx 的自定义 BEGIN 中途报错时会把处于失败事务的连接还回池，下一个使用者收到 25P02）

- 阻塞版的 `timeout` 必须是整数毫秒（`lock_timeout` 的单位）。0 分流到 `try_*`；带不足 1 毫秒零头的、不足 1 毫秒的（转成 0 在 PG 里是不限时）、
  超过 `i32::MAX` 毫秒的（`lock_timeout` 的上限，约 24.8 天）开事务之前就返回 `LockErr::InvalidTimeout`，不发 SQL

- 每把锁占一条连接（事务一直开着），持锁时间就是事务时间；连接数的预算见上面的配置
- 进程崩溃、连接断开，PG 中止事务，锁跟着释放，不需要租约
- `try_lock_i32` 用原生的 `pg_try_advisory_xact_lock(int, int)`：PG 上和 `try_lock_i64` 是两个 key 空间，
  和 core 的默认实现（合成一个 i64）不同。`pg_locks` 里看到的是 `classid = 锁类型, objid = key2, objsubid = 2`
- 字符串 key（feature `lock-row`）：BEGIN（同上）──> `INSERT INTO {p}lock (key) VALUES ($1)`，不提交
  - 别人持有（插了没提交）时撞主键，等它的事务结束：对方回滚就插入成功、拿到锁；多个人在等时只有一个插入成功，其余的接着等新的持有者
  - 55P03 ──> `Ok(None)`。INSERT 没有 `NOWAIT` / `SKIP LOCKED`，`try_lock` 用最小的 1 毫秒（0 是不限时），
    没人持有时不等，有人持有时约 1 毫秒后返回；没拿到时 PG 服务端日志记一条 lock timeout 的 ERROR
  - 释放是回滚，行跟着撤掉，表一直是空的。不用"行先提交、再 `FOR UPDATE`"：那样每个用过的 key 留一行，
    按订单号这类 key 加锁时表一直增长，而且行一旦被删，`FOR UPDATE` 查不到行也会被当成拿到锁
- 没开 `lock-row` 时字符串 key 哈希降级：`LockKey::hashed_i64()`（`string::xxh3_64`）转成 i64，走上面 `try_lock_i64` 的 advisory lock，
  不建表；guard 的 key 显示成 `advisory <数字> (hashed <原 key>)`。
  - 有损：不同 key 可能撞到同一个数，互相多锁（不会少锁）；和直接传数字的 `try_lock_i64` 共用一个空间
  - **开不开 `lock-row`，同一个 key 锁的是两样东西，互不互斥**：所有实例的 feature 要一致，切换 `lock-row` 的发布窗口里
    新旧实例对同一个 key 不互斥（停机切换，或保证切换期间不会同时加锁）
  - feature 在整个依赖图里合并，任何一个依赖开了 `hygiea-db/lock-row` 就是锁表实现，用 `cargo tree -e features` 确认

### 锁表

```sql
CREATE UNLOGGED TABLE {p}lock (
    key text PRIMARY KEY
)
```

- `{p}` 是 `table_prefix`（不写用应用名加 `_`，可以是空的）。建在锁的连接 search_path 的第一个 schema（`[db.lock.connect]` 可以换）。建表规则和动态配置一样（共用 `sql/` 的规则）：表在、列对上且是 UNLOGGED 就用；不在就在事务里
  加 advisory lock `(builtin::LOCK_TYPE, Lock::LockTableDdl)`、再查一遍后建；列对不上或不是 UNLOGGED 不动它，拒绝启动（`RepoErr::LockTableSchemaMismatch`），DDL 打在日志里
- 启动时（`from_config`）建；`connect_lazy` 时第一次用字符串 key 时建
- 行永远不提交，表一直是空的。UNLOGGED：插入不写 WAL；崩溃后清空、不复制到从库，对空表没有影响
- 持有中的行别的会话看不到，查"谁持有某个 key"只能从 `pg_locks` 的 transactionid 等待关系反推
- 不要手动往表里插入并提交：撞上已提交的行时 INSERT 立即报唯一约束冲突，这个 key 一直拿不到（`LockErr::AcquireFailed`）
- 已建好的表不会自动升级，改结构要同时改 `sql/postgres/lock.sql` 和 `sql/postgres/lock.rs` 的 `TABLE_COLUMNS`，已有的库手动改

## 4. 进程内的锁：`TokioLock`（hygiea-core，feature `lock-tokio`）

- `TokioLock::default()` 建，clone 出来的共用一张 key → `tokio::sync::Mutex` 的表（`DashMap`），key 精确匹配；
  别的进程、另外建的 `TokioLock` 看不到
- 只实现 `try_lock` / `lock`，整数 key 走默认实现转成字符串 key
- 阻塞版直接等 Mutex（先到先得），不轮询
- key 没人持有、没人在等时从表里删掉，表不会随 key 的个数一直长

## 5. 还没做的

- 方法分组和命名（`lock` 一直等、`try_lock_*_timeout`）、`WaitInterrupted`：讨论记录和实测数据见 [docs/todo/lock.md](../todo/lock.md)
- Redis：`SET NX PX` + token，释放用 Lua 校验 token；要不要看门狗续期待定
- 阻塞版的默认实现是轮询（最多 100ms 一次）：释放后最多 100ms 才发现、不公平、频繁打后端。有原生等待的后端覆盖它（PG、`TokioLock` 已覆盖）
