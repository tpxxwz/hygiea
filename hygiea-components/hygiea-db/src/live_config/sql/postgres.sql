-- hygiea 动态配置的表、函数、触发器（PostgreSQL 11+），建在 search_path 的第一个 schema 里。{p} 是前缀（应用名加 _）。
-- 第一次 bind 时一个都不存在才执行；手动执行时把 {p} 换掉。
-- 函数都 SET search_path FROM CURRENT：触发器在改表的那个会话里执行，固定成建表时的 search_path，
-- 不带 schema 的表名、函数名才找得对（别的程序 search_path 不一样也能改表）

CREATE TABLE {p}config (
    namespace  text        NOT NULL,
    key        text        NOT NULL,
    value      jsonb       NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (namespace, key)
);

CREATE TABLE {p}config_revision (
    namespace text   PRIMARY KEY,
    revision  bigint NOT NULL
);

CREATE TABLE {p}config_history (
    id         bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    namespace  text        NOT NULL,
    key        text        NOT NULL,
    op         char(1)     NOT NULL,
    old_value  jsonb,
    new_value  jsonb,
    revision   bigint      NOT NULL,
    changed_at timestamptz NOT NULL DEFAULT now(),
    changed_by text        NOT NULL DEFAULT current_user
);

CREATE INDEX {p}config_history_namespace_idx ON {p}config_history (namespace, id);

-- 记一次变更：revision +1（行锁让同一个 namespace 的版本号按提交顺序递增）、写历史、发通知
CREATE FUNCTION {p}config_record(ns text, k text, op char(1), old_value jsonb, new_value jsonb)
RETURNS void LANGUAGE plpgsql SET search_path FROM CURRENT AS $$
DECLARE
    rev bigint;
BEGIN
    INSERT INTO {p}config_revision AS r (namespace, revision) VALUES (ns, 1)
    ON CONFLICT (namespace) DO UPDATE SET revision = r.revision + 1
    RETURNING r.revision INTO rev;
    INSERT INTO {p}config_history (namespace, key, op, old_value, new_value, revision)
    VALUES (ns, k, op, old_value, new_value, rev);
    PERFORM pg_notify('{p}config', ns || ':' || rev);
END $$;

-- 改 namespace 或 key 算删掉旧的、插入新的；只改值算更新；值没变不算变更
CREATE FUNCTION {p}config_on_change()
RETURNS trigger LANGUAGE plpgsql SET search_path FROM CURRENT AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        PERFORM {p}config_record(NEW.namespace, NEW.key, 'I', NULL, NEW.value);
    ELSIF TG_OP = 'DELETE' THEN
        PERFORM {p}config_record(OLD.namespace, OLD.key, 'D', OLD.value, NULL);
    ELSIF (OLD.namespace, OLD.key) IS DISTINCT FROM (NEW.namespace, NEW.key) THEN
        PERFORM {p}config_record(OLD.namespace, OLD.key, 'D', OLD.value, NULL);
        PERFORM {p}config_record(NEW.namespace, NEW.key, 'I', NULL, NEW.value);
    ELSIF OLD.value IS DISTINCT FROM NEW.value THEN
        PERFORM {p}config_record(NEW.namespace, NEW.key, 'U', OLD.value, NEW.value);
    END IF;
    RETURN NULL;
END $$;

CREATE FUNCTION {p}config_touch()
RETURNS trigger LANGUAGE plpgsql SET search_path FROM CURRENT AS $$
BEGIN
    IF OLD.value IS DISTINCT FROM NEW.value THEN
        NEW.updated_at := now();
    END IF;
    RETURN NEW;
END $$;

CREATE TRIGGER {p}config_on_change
    AFTER INSERT OR UPDATE OR DELETE ON {p}config
    FOR EACH ROW EXECUTE FUNCTION {p}config_on_change();

CREATE TRIGGER {p}config_touch
    BEFORE UPDATE ON {p}config
    FOR EACH ROW EXECUTE FUNCTION {p}config_touch();
