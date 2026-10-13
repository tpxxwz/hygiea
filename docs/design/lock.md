# 分布式锁：trait、key 的注册、PG 实现

trait 和 key 在 hygiea-core 的 `lock` 模块（feature `lock`），后端在组件里实现：目前只有 hygiea-db 的
PostgreSQL 连接池（feature `lock`、`lock-row`）。SQLite、Redis 还没有实现。

---

## 1. trait

```rust
#[async_trait]
pub trait DistributedLock: Send + Sync {
    async fn try_lock(&self, key: LockKey) -> Result<Option<LockGuard>>;              // 后端必须实现
    async fn try_lock_i64(&self, key: impl Into<LockKeyI64> + Send) -> ..;             // 默认：999999999:<数字> 调 try_lock
    async fn try_lock_i32(&self, key1: impl Into<LockType> + Send,
                          key2: impl Into<LockKeyI32> + Send) -> ..;                   // 默认：高 32 位锁类型、低 32 位 key2 合成 i64
    async fn lock(&self, key: LockKey, timeout: Duration) -> ..;                       // 默认：轮询 try_lock（最多 100ms 一次）
    async fn lock_i64(..) / lock_i32(..)                                               // 默认：一层调一层
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

### 编号

规则和错误码一样，代码也共用（hygiea-macros 的 `prefix.rs`、`template.rs`）：

- 9 位 = 项目前缀 3 位（Cargo.toml 的 `err_code_project_prefix`，和错误码共用）+ 模块前缀 + 编号
  - `#[lock_module_prefix = "02"]`：2 位，编号 4 位，给使用 hygiea 的项目
  - `#[lock_internal_module_prefix = "003"]`：3 位，编号 3 位，只给 hygiea 自己（项目前缀 999）
  - 都不写：编号 6 位
- `hy_lock_type` 和 `hy_lock_key` 共用一套编号、一个注册表（`LOCK_REGISTRATIONS`），和错误码是两套
- 查重：同一个 enum 里宏报错；同模块靠常量 `HYGIEA_LOCK_<9 位>` 重名（E0428）；跨 crate 靠导出符号
  `__hygiea_lock_<9 位>`（Linux 链接期），都没拦住的由 core 的 ctor 启动时查重，打一行报错后退出
- `999999999` 是内置的：`try_lock_i64` 默认实现转成的字符串 key 用它，手动注册在 core 里

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

`SqlxPgPool`、`SeaOrmPgPool`（用底层的 sqlx 池）直接实现 `DistributedLock`，实现只有一份（`lock/postgres.rs` 的 `PgLock`，
`impl_pg_lock!` 给两个连接池生成 impl）。

```text
try_lock_i64(k)  ── 主池 begin ──> SELECT pg_try_advisory_xact_lock(k)
                                    ├─ true  ──> guard 持有事务；release / drop 时 ROLLBACK，锁随事务释放
                                    └─ false ──> 丢掉事务（连接归还时回滚），Ok(None)
lock_i64(k, t)   ── begin ──> set_config('lock_timeout', t, true) ──> SELECT pg_advisory_xact_lock(k)
                                    └─ 55P03（lock_not_available）──> Ok(None)
```

- 每把锁占一条连接（事务一直开着），同时持有的锁加上业务查询要在 `max_connections` 以内；持锁时间就是事务时间
- 进程崩溃、连接断开，PG 中止事务，锁跟着释放，不需要租约
- `try_lock_i32` 用原生的 `pg_try_advisory_xact_lock(int, int)`：PG 上和 `try_lock_i64` 是两个 key 空间，
  和 core 的默认实现（合成一个 i64）不同。`pg_locks` 里看到的是 `classid = 锁类型, objid = key2, objsubid = 2`
- 字符串 key（feature `lock-row`）：
  1. `INSERT INTO {app}_lock (key) VALUES ($1) ON CONFLICT DO NOTHING`（自动提交；已有的行不加锁，别人持有时也不等）
  2. 事务里 `SELECT key .. FOR UPDATE SKIP LOCKED`，拿到行就是拿到锁；阻塞版 `FOR UPDATE` + `lock_timeout`
- 没开 `lock-row` 时字符串 key 返回 `LockErr::Unsupported`

### 锁表

```sql
CREATE TABLE {app_name}_lock (
    key        text        PRIMARY KEY,
    created_at timestamptz NOT NULL DEFAULT now()
)
```

- 建在 search_path 的第一个 schema。建表规则和动态配置一样（共用 `ddl.rs`）：表在且列对上就用；不在就在事务里
  加 advisory lock、再查一遍后建；列对不上不动它，拒绝启动（`RepoErr::LockTableSchemaMismatch`），DDL 打在日志里
- 启动时（`from_config`）建；`connect_lazy` 时第一次用字符串 key 时建
- 一个 key 一行，加锁时补上，不删
- 已建好的表不会自动升级，改结构要同时改 `sql/postgres.sql` 和 `table.rs` 的 `EXPECTED`，已有的库手动改

## 4. 还没做的

- SQLite：持有写事务做锁会挡住整个库的写（SQLite 同一时间只有一个写事务）；租约行（插入带过期时间的行，释放时删）
  不挡写，但要有过期时间，崩溃靠过期回收，持锁超过过期时间会被别人拿走。方案定了再做
- Redis：`SET NX PX` + token，释放用 Lua 校验 token；要不要看门狗续期待定
- 阻塞版的默认实现是轮询（最多 100ms 一次）：释放后最多 100ms 才发现、不公平、频繁打后端。有原生等待的后端覆盖它（PG 已覆盖）
