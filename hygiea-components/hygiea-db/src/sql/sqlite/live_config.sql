-- hygiea 动态配置的表、触发器（SQLite 3.35+）。{p} 是前缀（应用名加 _），时间存 UTC 的 RFC 3339 文本。
-- 第一次 bind 时一个都不存在才执行；手动执行时把 {p} 换掉

CREATE TABLE {p}config (
    namespace  TEXT NOT NULL,
    key        TEXT NOT NULL,
    value      TEXT NOT NULL CHECK (json_valid(value)),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (namespace, key)
);

CREATE TABLE {p}config_revision (
    namespace TEXT    PRIMARY KEY,
    revision  INTEGER NOT NULL
);

CREATE TABLE {p}config_history (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    namespace  TEXT    NOT NULL,
    key        TEXT    NOT NULL,
    op         TEXT    NOT NULL,
    old_value  TEXT,
    new_value  TEXT,
    revision   INTEGER NOT NULL,
    changed_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX {p}config_history_namespace_idx ON {p}config_history (namespace, id);

-- 每次变更：revision +1、写历史。改 namespace 或 key 算删掉旧的、插入新的；只改值算更新；值没变不算变更

CREATE TRIGGER {p}config_insert AFTER INSERT ON {p}config
BEGIN
    INSERT INTO {p}config_revision (namespace, revision) VALUES (NEW.namespace, 1)
    ON CONFLICT (namespace) DO UPDATE SET revision = revision + 1;
    INSERT INTO {p}config_history (namespace, key, op, old_value, new_value, revision)
    SELECT NEW.namespace, NEW.key, 'I', NULL, NEW.value, revision
    FROM {p}config_revision WHERE namespace = NEW.namespace;
END;

CREATE TRIGGER {p}config_delete AFTER DELETE ON {p}config
BEGIN
    INSERT INTO {p}config_revision (namespace, revision) VALUES (OLD.namespace, 1)
    ON CONFLICT (namespace) DO UPDATE SET revision = revision + 1;
    INSERT INTO {p}config_history (namespace, key, op, old_value, new_value, revision)
    SELECT OLD.namespace, OLD.key, 'D', OLD.value, NULL, revision
    FROM {p}config_revision WHERE namespace = OLD.namespace;
END;

CREATE TRIGGER {p}config_update AFTER UPDATE ON {p}config
WHEN OLD.namespace = NEW.namespace AND OLD.key = NEW.key AND OLD.value IS NOT NEW.value
BEGIN
    INSERT INTO {p}config_revision (namespace, revision) VALUES (NEW.namespace, 1)
    ON CONFLICT (namespace) DO UPDATE SET revision = revision + 1;
    INSERT INTO {p}config_history (namespace, key, op, old_value, new_value, revision)
    SELECT NEW.namespace, NEW.key, 'U', OLD.value, NEW.value, revision
    FROM {p}config_revision WHERE namespace = NEW.namespace;
    UPDATE {p}config SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
    WHERE namespace = NEW.namespace AND key = NEW.key;
END;

CREATE TRIGGER {p}config_rename AFTER UPDATE ON {p}config
WHEN OLD.namespace IS NOT NEW.namespace OR OLD.key IS NOT NEW.key
BEGIN
    INSERT INTO {p}config_revision (namespace, revision) VALUES (OLD.namespace, 1)
    ON CONFLICT (namespace) DO UPDATE SET revision = revision + 1;
    INSERT INTO {p}config_history (namespace, key, op, old_value, new_value, revision)
    SELECT OLD.namespace, OLD.key, 'D', OLD.value, NULL, revision
    FROM {p}config_revision WHERE namespace = OLD.namespace;
    INSERT INTO {p}config_revision (namespace, revision) VALUES (NEW.namespace, 1)
    ON CONFLICT (namespace) DO UPDATE SET revision = revision + 1;
    INSERT INTO {p}config_history (namespace, key, op, old_value, new_value, revision)
    SELECT NEW.namespace, NEW.key, 'I', NULL, NEW.value, revision
    FROM {p}config_revision WHERE namespace = NEW.namespace;
    UPDATE {p}config SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
    WHERE namespace = NEW.namespace AND key = NEW.key;
END;
