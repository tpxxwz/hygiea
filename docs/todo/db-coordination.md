# 数据库协调能力待办

为只依赖数据库、不额外部署 Redis、Kafka 等基础设施的服务，考虑提供基于数据库辅助表的事件监听和分布式锁能力。目标是给 PostgreSQL、MySQL、SQLite 提供尽量一致的上层接口，同时明确各数据库原生能力和可靠性差异。

## 事件辅助表

### 目标

- 业务事务内把变更事件写入辅助表，保证业务数据和事件同时提交或同时回滚
- 消费者按递增 ID、时间或游标拉取事件，支持断线后继续消费，避免只依赖瞬时通知造成事件丢失
- PostgreSQL 可用 `LISTEN/NOTIFY` 唤醒消费者，但辅助表仍作为可靠事件源
- MySQL、SQLite 没有等价的 `LISTEN/NOTIFY`，默认轮询辅助表；以后可按后端增加 binlog CDC 或连接级 hook 作为唤醒优化
- 支持多个消费者、失败重试、幂等处理、事件保留和清理

### 表结构草案

```sql
CREATE TABLE hygiea_db_event (
    id          BIGINT PRIMARY KEY,
    topic       VARCHAR(255) NOT NULL,
    event_key   VARCHAR(255),
    payload     TEXT NOT NULL,
    created_at  TIMESTAMP NOT NULL,
    expires_at  TIMESTAMP NULL
);
```

不同数据库的自增主键、时间类型和 JSON 类型由 migration 分别处理。事件本身只追加，不原地修改；消费进度另放 checkpoint 表，避免多个消费组互相影响。

```sql
CREATE TABLE hygiea_db_event_checkpoint (
    consumer_group VARCHAR(255) PRIMARY KEY,
    last_event_id  BIGINT NOT NULL,
    updated_at     TIMESTAMP NOT NULL
);
```

### 要定的

- API 放在 `hygiea-db` 的公共抽象中，还是分别放进各数据库实现
- 事件 ID 使用数据库自增、UUIDv7，还是由框架生成的有序 ID
- 是否提供 Trigger 建表方案；应用主动写 outbox 更容易携带明确的业务 payload，Trigger 能覆盖绕过应用的写入
- 消费进度按事件确认还是批量确认；并发消费者如何认领事件
- 清理策略按时间、已确认水位或两者结合；慢消费者是否阻止清理
- PostgreSQL `NOTIFY` 只作为唤醒信号，payload 只传最新 ID 或 topic，收到后仍查询辅助表
- MySQL 后续是否增加 binlog CDC adapter；SQLite 的 update hook 仅对设置 hook 的连接有效，不能作为可靠跨连接订阅

## 数据库分布式锁

### 目标

- 在已有数据库上提供跨进程互斥，用于定时任务选主、单实例迁移和短时间临界区
- 锁要有明确 owner、租约期限和 fencing token，避免持锁进程暂停后继续以旧身份写入
- 获取、续租、释放均使用数据库原子条件更新；释放时必须校验 owner 和 fencing token

### 表结构草案

```sql
CREATE TABLE hygiea_db_lock (
    lock_key       VARCHAR(255) PRIMARY KEY,
    owner_id       VARCHAR(255) NOT NULL,
    fencing_token  BIGINT NOT NULL,
    lease_until    TIMESTAMP NOT NULL,
    updated_at     TIMESTAMP NOT NULL
);
```

获取逻辑需要按数据库方言实现原子 upsert：锁不存在或租约已过期时才能写入新 owner，并递增 `fencing_token`。续租和释放必须带 `lock_key + owner_id + fencing_token` 条件。

### 要定的

- 优先包装数据库原生锁，还是统一使用辅助表：PostgreSQL advisory lock、MySQL `GET_LOCK` 与 SQLite 的事务锁语义不同，辅助表更容易提供一致 API
- 租约时间以数据库时间为准，避免应用节点时钟漂移；需要确认各后端获取当前时间和精度的差异
- 锁句柄 drop 时只做尽力释放，正确性不能依赖异步 drop；公开显式 `release()`
- 是否内置后台续租；若内置，需要定义续租失败、连接断开和任务取消时句柄的状态
- fencing token 如何交给受保护资源校验；如果资源不校验 token，租约锁只能提供尽力互斥
- SQLite 主要面向同一数据库文件上的多进程协调，不承诺网络文件系统上的一致性

## 建议的实现顺序

1. 先定义后端无关的事件、checkpoint、锁记录和 trait，不隐藏可靠性语义。
2. 先实现 PostgreSQL 和 MySQL 的辅助表轮询/锁，再实现 SQLite。
3. PostgreSQL 增加 `LISTEN/NOTIFY` 唤醒优化；轮询保留为补漏和断线恢复机制。
4. 加故障测试：事务回滚、消费者崩溃重启、通知丢失、锁过期后旧 owner 恢复、并发抢锁和续租失败。

