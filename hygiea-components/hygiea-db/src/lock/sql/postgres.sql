-- hygiea 分布式锁的锁表（feature lock-row），建在 search_path 的第一个 schema 里。{p} 是前缀（应用名加 _）。
-- 字符串 key 一行，加锁时补上，不删；加锁拿的是这一行的行锁。
-- 表不存在时才执行；手动执行时把 {p} 换掉。改了列要同时改 table.rs 的 EXPECTED

CREATE TABLE {p}lock (
    key        text        PRIMARY KEY,
    created_at timestamptz NOT NULL DEFAULT now()
);
