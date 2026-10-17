# 事务隔离级别待办

现在的事务函数见 `hygiea-db` 的 `seaorm::transaction*`、`sqlx::transaction`。

## 定下来的做法：不做跨框架、跨数据库的统一枚举

同名的隔离级别在不同数据库里语义不同，统一成一套枚举只是写法一样，换库后行为会变：

- PostgreSQL 的 REPEATABLE READ 是快照隔离：整个事务读同一个快照，没有幻读；并发更新同一行时后提交的报序列化错误
- MySQL 的 REPEATABLE READ：普通 SELECT 读快照，`SELECT ... FOR UPDATE` 这类加锁读读最新数据，还加间隙锁
- PostgreSQL 的 READ UNCOMMITTED 实际按 READ COMMITTED 执行，不会脏读
- SQLite 没有隔离级别可选（本身接近串行化），只有 `BEGIN DEFERRED / IMMEDIATE / EXCLUSIVE` 控制加锁时机

SeaORM 2.0 的处理（看源码确认的）：

- PostgreSQL：先 `BEGIN`，再在事务里 `SET TRANSACTION ...`
- MySQL：先 `SET TRANSACTION ...`，再 `BEGIN`
- SQLite：SeaORM 源码 driver/sqlx_sqlite.rs 的 `set_transaction_config` 只打 `warn!` 然后忽略，不报错；
  SQLite 的 BEGIN 模式另走 `SqliteTransactionMode`，不在 `begin_with_config` 里

所以每个框架的封装各用自己的类型，语义和限制以框架文档为准。

## 待办

- [ ] sqlx 加 `transaction_with(pool, begin_sql, op, f)`：照 sqlx 0.9 的 `Pool::begin_with(statement)`，
      调用方传对应数据库的 `BEGIN ...` 原生语句（比如 PostgreSQL 的 `BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY`、
      SQLite 的 `BEGIN IMMEDIATE`）。用原生 SQL，用的是哪个库的语义一眼看得出来。提交、回滚、错误处理和 `transaction` 一样
- [ ] `seaorm::transaction_with_config` 的文档补一句：SQLite 下 SeaORM 只警告不生效，要控制加锁时机用 `SqliteTransactionMode`
- [ ] sqlx 的 `transaction_with` 加容器测试：事务里 `SHOW transaction_isolation` / `SHOW transaction_read_only`，确认真开出了指定的级别
      （SeaORM 的已经有了，`tests/seaorm_postgres.rs`）
- [ ] 嵌套事务的传播方式：`db` 已经是事务时 SeaORM 开 savepoint（相当于 Spring 的 `NESTED`），内层出错只回滚到 savepoint。
      Spring 默认是 `REQUIRED`（加入外层，内层出错整个回滚）。`ConnectionTrait` 分不出连接池和事务，要做 `REQUIRED`
      得 hygiea 自己定一个 trait：连接池上开新事务再提交，事务上直接复用
