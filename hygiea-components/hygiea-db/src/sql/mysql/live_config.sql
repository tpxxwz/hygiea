-- hygiea 动态配置的表、触发器（MySQL 8.0+），建在连接的当前库（DATABASE()）里。{p} 是表名前缀（默认应用名加 _，可以是空的）。
-- 第一次 bind 时一个都不存在才执行；手动执行时把 {p} 换掉，按 "-- @@" 分开逐条执行（触发器里有分号，
-- 用 mysql 命令行时要先改 DELIMITER）。
-- namespace、key 用 utf8mb4_bin：区分大小写，和 PG、SQLite 一致。时间是会话时区的 CURRENT_TIMESTAMP

CREATE TABLE {p}config (
    namespace  VARCHAR(255) COLLATE utf8mb4_bin NOT NULL,
    `key`      VARCHAR(255) COLLATE utf8mb4_bin NOT NULL,
    value      JSON         NOT NULL,
    updated_at DATETIME(6)  NOT NULL DEFAULT CURRENT_TIMESTAMP(6) ON UPDATE CURRENT_TIMESTAMP(6),
    PRIMARY KEY (namespace, `key`)
)
-- @@
CREATE TABLE {p}config_revision (
    namespace VARCHAR(255) COLLATE utf8mb4_bin PRIMARY KEY,
    revision  BIGINT       NOT NULL
)
-- @@
CREATE TABLE {p}config_history (
    id         BIGINT       AUTO_INCREMENT PRIMARY KEY,
    namespace  VARCHAR(255) COLLATE utf8mb4_bin NOT NULL,
    `key`      VARCHAR(255) COLLATE utf8mb4_bin NOT NULL,
    op         CHAR(1)      NOT NULL,
    old_value  JSON,
    new_value  JSON,
    revision   BIGINT       NOT NULL,
    changed_at DATETIME(6)  NOT NULL DEFAULT CURRENT_TIMESTAMP(6),
    -- USER()：发起修改的连接的账号（触发器里 CURRENT_USER() 是触发器的定义者，不用它）
    changed_by VARCHAR(288) NOT NULL,
    INDEX {p}config_history_namespace_idx (namespace, id)
)
-- @@
-- 每次变更：revision +1、写历史。改 namespace 或 key 算删掉旧的、插入新的；只改值算更新；值没变不算变更。
-- revision 那一行的行锁让并发的变更按提交顺序递增
CREATE TRIGGER {p}config_insert AFTER INSERT ON {p}config FOR EACH ROW
BEGIN
    INSERT INTO {p}config_revision (namespace, revision) VALUES (NEW.namespace, 1)
    ON DUPLICATE KEY UPDATE revision = revision + 1;
    INSERT INTO {p}config_history (namespace, `key`, op, old_value, new_value, revision, changed_by)
    SELECT NEW.namespace, NEW.`key`, 'I', NULL, NEW.value, revision, USER()
    FROM {p}config_revision WHERE namespace = NEW.namespace;
END
-- @@
CREATE TRIGGER {p}config_delete AFTER DELETE ON {p}config FOR EACH ROW
BEGIN
    INSERT INTO {p}config_revision (namespace, revision) VALUES (OLD.namespace, 1)
    ON DUPLICATE KEY UPDATE revision = revision + 1;
    INSERT INTO {p}config_history (namespace, `key`, op, old_value, new_value, revision, changed_by)
    SELECT OLD.namespace, OLD.`key`, 'D', OLD.value, NULL, revision, USER()
    FROM {p}config_revision WHERE namespace = OLD.namespace;
END
-- @@
CREATE TRIGGER {p}config_update AFTER UPDATE ON {p}config FOR EACH ROW
BEGIN
    IF OLD.namespace <> NEW.namespace OR OLD.`key` <> NEW.`key` THEN
        INSERT INTO {p}config_revision (namespace, revision) VALUES (OLD.namespace, 1)
        ON DUPLICATE KEY UPDATE revision = revision + 1;
        INSERT INTO {p}config_history (namespace, `key`, op, old_value, new_value, revision, changed_by)
        SELECT OLD.namespace, OLD.`key`, 'D', OLD.value, NULL, revision, USER()
        FROM {p}config_revision WHERE namespace = OLD.namespace;
        INSERT INTO {p}config_revision (namespace, revision) VALUES (NEW.namespace, 1)
        ON DUPLICATE KEY UPDATE revision = revision + 1;
        INSERT INTO {p}config_history (namespace, `key`, op, old_value, new_value, revision, changed_by)
        SELECT NEW.namespace, NEW.`key`, 'I', NULL, NEW.value, revision, USER()
        FROM {p}config_revision WHERE namespace = NEW.namespace;
    ELSEIF NOT (OLD.value <=> NEW.value) THEN
        INSERT INTO {p}config_revision (namespace, revision) VALUES (NEW.namespace, 1)
        ON DUPLICATE KEY UPDATE revision = revision + 1;
        INSERT INTO {p}config_history (namespace, `key`, op, old_value, new_value, revision, changed_by)
        SELECT NEW.namespace, NEW.`key`, 'U', OLD.value, NEW.value, revision, USER()
        FROM {p}config_revision WHERE namespace = NEW.namespace;
    END IF;
END
