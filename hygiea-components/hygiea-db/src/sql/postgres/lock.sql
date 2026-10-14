-- hygiea 分布式锁的锁表（feature lock-row），建在 search_path 的第一个 schema 里。{p} 是前缀（应用名加 _）。
-- 加锁是在事务里插入 key 那一行、不提交，别人插同一个 key 时撞主键要等这个事务结束；释放就是回滚，行跟着撤掉。
-- 行永远不提交，表一直是空的，所以用 UNLOGGED：不写 WAL，崩溃后被清空、不复制到从库都没有影响。
-- 表不存在时才执行；手动执行时把 {p} 换掉。改了列要同时改 lock.rs 的 TABLE_COLUMNS

CREATE UNLOGGED TABLE {p}lock (
    key text PRIMARY KEY
);
