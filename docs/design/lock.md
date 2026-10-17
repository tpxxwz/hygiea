# 分布式锁：内部实现和取舍

用法、行为和配置都在模块文档里，这里不重复：

- trait、key、编号、`TokioLock`：`hygiea-core/src/lock/mod.rs`（docs.rs 上是 `hygiea::lock`）
- PG / MySQL 的行为、限制、`lock-row`：`hygiea-components/hygiea-db/src/lock/mod.rs`（`hygiea::db::lock`），配置见 `PgLockConfig` / `MySqlLockConfig`

这里只讲内部怎么做、为什么这样做。还没做的、没定的在 [docs/todo/lock.md](../todo/lock.md)。

---

## 1. key 和编号

- `LockType`、`LockKey` 的字段私有，只能由宏生成，随手写的数字、字符串编译不过。
- `LockKeyI64::xxh3_64(&key)?` 放在 `LockKeyI64` 上而不是 `LockKey` 上，也不做 `Into<LockKeyI64>`，免得 `try_lock_i64(lock_key!(..))` 悄悄哈希：
  调用方写出 `xxh3_64` 才算选了降级。碰撞概率：n 个 key 里至少一对碰撞约 n² / 2⁶⁵（100 万个约 3 × 10⁻⁸）
- 编号的规则和错误码一样，代码也共用（hygiea-macros 的 `prefix.rs`、`template.rs`）；`hy_lock_type` 和 `hy_lock_key` 共用一套编号、
  一个注册表（`LOCK_REGISTRATIONS`），和错误码是两套
- 查重：同一个 enum 里宏报错；同模块靠常量 `HYGIEA_LOCK_<9 位>` 重名（E0428）；跨 crate 靠导出符号
  `__hygiea_lock_<9 位>`（Linux 链接期），都没拦住的由 core 的 ctor 启动时查重，打一行报错后退出
- `999999999` 是 hygiea 内置保留的（`hygiea_core::builtin::LOCK_TYPE` / `LOCK_CODE`，常开、文档里隐藏），注册表里手动占着一条，
  业务注册不到。用在两处，互不影响：`try_lock_i64` 默认实现转成的字符串 key 的前缀（`999999999:<数字>`）；hygiea 组件自己用的锁
  `builtin::Lock`，比如 hygiea-db 建表时串行化的 `pg_advisory_xact_lock(999999999, Lock::LiveConfigDdl / LockTableDdl)`
  （`(int, int)` 空间，和业务 `try_lock_i64` 的 bigint 空间分开，业务的锁类型又注册不到 999 段，所以不会撞）。
  `builtin` 不跟 `lock` feature，只开 `db-live-config` 不会带上锁
- `lock_key!` 和 `err!` 共用 `__hygiea_tpl_args!`：模板语法、写法和变量个数、`{ .. }` 里的字面量 key 在编译期检查；
  key 用了表达式时查不了，运行时缺变量由 `LockKey::as_str()` 报错

### 拦不住的

- `lock::__private::registered` / `lock_key` 是 `pub`（宏在调用方 crate 里展开要用），直接调能造出没注册过的 key，
  和 `HyErr::new` 一样只靠"不用 `__private`"的约定
- 绕过 hygiea 直接操作库（手写 `pg_advisory_lock`、改锁表）

## 2. hygiea-db 的 `DbLockClient`

`DbLockClient` 里面是哪种数据库由建它的组件决定（`Pg(PgLock)` / `MySql(MySqlLock)`），按数据库分发。
实现每种数据库只有一份（`lock/sqlx/postgres.rs`、`lock/sqlx/mysql.rs`），sqlx、SeaORM 的组件共用（SeaORM 交出底层的 sqlx 池）。

## 3. PostgreSQL

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
- 为什么是每个事务里 `SET LOCAL`，不是锁专用池的启动参数、单独的锁用户、要求 DBA 设成 0 或启动时校验（实测见 git 历史里的
  `docs/todo/lock.md` 调查记录二）：启动参数经 PgBouncer transaction 模式会被拒或悄悄丢掉，而框架检测不出中间有没有 PgBouncer；
  单独用户要 DBA 多一套账号；要求库里是 0 会让业务查询失去保护。`SET LOCAL` 直连、PgBouncer 都生效，回滚后恢复原值、不泄漏给下一个客户端，
  和 BEGIN 合在一条消息里不增加往返
- READ COMMITTED 是固定的（不跟着库的 `default_transaction_isolation` 走）：REPEATABLE READ / SERIALIZABLE 的快照留着 xmin，会挡住同库的 vacuum
- 不用 sqlx 的 `Transaction`，自己取连接、发 BEGIN / ROLLBACK：BEGIN 那条消息中途报错、加锁出错、等锁时被取消、回滚失败，
  连接都直接关掉不还回池（sqlx 的自定义 BEGIN 中途报错时会把处于失败事务的连接还回池，下一个使用者收到 25P02）
- `try_lock_i32` 用原生的 `(int, int)` 形式，`pg_locks` 里看到的是 `classid = 锁类型, objid = key2, objsubid = 2`

### 字符串 key：锁表（`lock-row`）

BEGIN（同上）──> `INSERT INTO {p}lock (key) VALUES ($1)`，不提交：

```sql
CREATE UNLOGGED TABLE {p}lock (
    key text PRIMARY KEY
)
```

- 别人持有（插了没提交）时撞主键，等它的事务结束：对方回滚就插入成功、拿到锁；多个人在等时只有一个插入成功，其余的接着等新的持有者
- INSERT 没有 `NOWAIT` / `SKIP LOCKED`，所以 `try_lock` 用最小的 1 毫秒 `lock_timeout`（0 是不限时）
- 不用"行先提交、再 `FOR UPDATE`"：那样每个用过的 key 留一行，按订单号这类 key 加锁时表一直增长，而且行一旦被删，
  `FOR UPDATE` 查不到行也会被当成拿到锁。选现在的写法是为了已提交的行数不随 key 的个数增长，不是为了写得少：
  `FOR UPDATE` 每次只写一条行锁记录、不留死元组；现在的写法每次留一个死元组和死索引项给 vacuum，UNLOGGED 后只剩 abort 记录写 WAL。
  两种写法持锁期间都分到 xid、都挡同库的 vacuum
- `DEFERRABLE` 唯一约束不行：插入时不等、提交时才检查，而锁永远回滚，等于不互斥
- 建表：在事务里加 advisory lock `(builtin::LOCK_TYPE, Lock::LockTableDdl)`、再查一遍后建；规则和动态配置共用 `sql/` 的检查。
  已建好的表不会自动升级，改结构要同时改 `sql/postgres/lock.sql` 和 `sql/postgres/lock.rs` 的 `TABLE_COLUMNS`，已有的库手动改
- 没开 `lock-row` 时字符串 key 返回 `Unsupported`，不悄悄换成别的实现：字符串 key 在 PG 上只有锁表这一种语义

## 4. MySQL：`GET_LOCK`

`lock/sqlx/mysql.rs` 的 `MySqlLock`，SQL 在 `sql/mysql/lock.rs`。

```text
try_lock(k)     ── 锁的池取连接 ──> SET SESSION wait_timeout = 上限, @hygiea_lock = GET_LOCK(name, 0) ──> SELECT @hygiea_lock
                                    ├─ 1 ──> guard 占着这条连接；release / drop 时 RELEASE_LOCK、wait_timeout 恢复默认，再还回池
                                    └─ 0 ──> 恢复 wait_timeout，还回池，Ok(None)
lock(k, t)      ── 校验 t ──> 同上，GET_LOCK(name, 秒数)：原生等待，对方一释放马上拿到
```

- 加锁、释放中途出错或被取消时，连接从池里摘下来直接断开，不还回池。不用 `close_on_drop`：sqlx 的 MySQL 连接要先读完上一条结果
  才发关闭，等锁时被取消会一直占着池的名额。断开即释放锁

## 5. 进程内的锁：`TokioLock`

key 没人持有、没人在等时从表里删掉（drop guard、等锁结束时检查），表不会随 key 的个数一直长。
