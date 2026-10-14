//! 调查：多个库统一放在 `[db.<名字>]` 下、用 `kind` 标签区分种类（serde 内部标签）时，
//! 和 hygiea 的 `load_config` 一样经 config-rs 加载（TOML 文件 + 环境变量 try_parsing + `--set`），
//! 变体里装真实的连接池配置（`deny_unknown_fields` + `default`），哪些写法能用、报错长什么样。
//! 对照组是现在的写法：结构体字段直接是 `SqlxPgConfig`。
//!
//! 结论（config 0.15.27、serde 1.0.229）：内部标签和 `deny_unknown_fields` 能一起用，但内部标签要先把整张表缓冲成
//! serde 自己的中间值，绕过了 config-rs 的反序列化器：`--set` 的字符串改不了数字、布尔字段，环境变量 try_parsing
//! 成整数的值（纯数字密码）进不了 String 字段，报错路径只到 `db.<名字>`。种类占一层（C：`[db.sqlx_pg.main]`）或
//! 外部标签（D：`[db.main.sqlx_pg]`）不经过缓冲，没有这些问题。另外 config-rs 把环境变量的 key 转成小写，
//! 名字只能用小写。最后没有采用：仍是应用自己的结构体字段，手写 `add_named`
//! ```sh
//! cargo test --test tagged_db_config -- --nocapture
//! ```

// 调查用：解析出来只看成不成功，字段不读
#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};

use hygiea::db::{SeaOrmPgConfig, SqlxPgConfig, SqlxSqliteConfig};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum DbEntry {
    SqlxPg(SqlxPgConfig),
    SeaormPg(SeaOrmPgConfig),
    SqlxSqlite(SqlxSqliteConfig),
}

#[derive(Debug, Deserialize)]
struct TaggedApp {
    db: BTreeMap<String, DbEntry>,
}

/// 对照组：现在的写法，字段类型固定
#[derive(Debug, Deserialize)]
struct FixedApp {
    db: FixedDbs,
}

#[derive(Debug, Deserialize)]
struct FixedDbs {
    main: SqlxPgConfig,
}

/// 和 `Registry::load_config` 一样的管道：文件 → 环境变量（前缀 HY，`__` 分隔，try_parsing）→ `--set`
fn load<T: serde::de::DeserializeOwned>(
    toml: &str,
    env: &[(&str, &str)],
    set: &[(&str, &str)],
) -> Result<T, config::ConfigError> {
    let env: HashMap<String, String> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let mut builder = config::Config::builder()
        .add_source(config::File::from_str(toml, config::FileFormat::Toml))
        .add_source(
            config::Environment::with_prefix("HY")
                .prefix_separator("__")
                .separator("__")
                .try_parsing(true)
                .source(Some(env)),
        );
    for (k, v) in set {
        builder = builder.set_override(*k, *v)?;
    }
    builder.build()?.try_deserialize()
}

const BASE: &str = r#"
[db.main]
kind = "sqlx-pg"
[db.main.connect]
host = "pg-main"
password = "secret"
[db.main.pool]
max_connections = 30

[db.orm]
kind = "seaorm-pg"
[db.orm.log]
enabled = false

[db.local]
kind = "sqlx-sqlite"
[db.local.connect]
database = "/tmp/a.db"
"#;

fn show<T: std::fmt::Debug>(label: &str, r: &Result<T, config::ConfigError>) {
    match r {
        Ok(_) => println!("[{label}] OK"),
        Err(e) => println!("[{label}] ERR: {e}"),
    }
}

/// 基本解析：各段、默认值、Option 数字都能进去
#[test]
fn basic() {
    let app: TaggedApp = load(BASE, &[], &[]).unwrap();
    let DbEntry::SqlxPg(main) = &app.db["main"] else {
        panic!()
    };
    assert_eq!(main.connect.host, "pg-main");
    assert_eq!(main.connect.port, 5432, "没写的用默认值");
    assert_eq!(main.pool.as_ref().unwrap().max_connections, Some(30));
    assert!(matches!(app.db["orm"], DbEntry::SeaormPg(_)));
    assert!(matches!(app.db["local"], DbEntry::SqlxSqlite(_)));
    // 直接用 toml（配置模板的单元测试走这条）
    let direct: TaggedApp = toml::from_str(BASE).unwrap();
    assert_eq!(direct.db.len(), 3);
}

/// deny_unknown_fields：变体顶层、嵌套段里写错字段名
#[test]
fn unknown_fields() {
    let top = load::<TaggedApp>(&format!("{BASE}\n[db.main.typo]\nx = 1\n"), &[], &[]);
    show("变体顶层多一段", &top);
    assert!(top.is_err());
    let nested = load::<TaggedApp>(
        &BASE.replace("host = \"pg-main\"", "host = \"pg-main\"\nhots = \"x\""),
        &[],
        &[],
    );
    show("connect 里拼错字段", &nested);
    assert!(nested.is_err());
    let fixed = load::<FixedApp>("[db.main.connect]\nhost = \"a\"\nhots = \"x\"\n", &[], &[]);
    show("对照组 connect 里拼错字段", &fixed);
}

/// kind 没写、写错
#[test]
fn bad_kind() {
    let missing = load::<TaggedApp>("[db.main.connect]\nhost = \"a\"\n", &[], &[]);
    show("没写 kind", &missing);
    assert!(missing.is_err());
    let unknown = load::<TaggedApp>("[db.main]\nkind = \"seaorm-mysql\"\n", &[], &[]);
    show("kind 写了没编译的种类", &unknown);
    assert!(unknown.is_err());
}

/// 环境变量 try_parsing：纯数字的密码会被解析成整数，字段是 String
#[test]
fn env_numeric_string() {
    let env = [("HY__DB__MAIN__CONNECT__PASSWORD", "123456")];
    let tagged = load::<TaggedApp>(BASE, &env, &[]);
    show("tagged 环境变量 password=123456", &tagged);
    let fixed = load::<FixedApp>("[db.main.connect]\nhost = \"a\"\n", &env, &[]);
    show("对照组 环境变量 password=123456", &fixed);
    if let Ok(app) = &fixed {
        assert_eq!(app.db.main.connect.password, "123456");
    }
}

/// 环境变量改数字、布尔：port（u16）、max_connections（Option<u32>）、connect_lazy（bool）
#[test]
fn env_numbers_and_bools() {
    let env = [
        ("HY__DB__MAIN__CONNECT__PORT", "5433"),
        ("HY__DB__MAIN__POOL__MAX_CONNECTIONS", "7"),
        ("HY__DB__MAIN__POOL__CONNECT_LAZY", "true"),
    ];
    let tagged = load::<TaggedApp>(BASE, &env, &[]);
    show("tagged 环境变量 数字/布尔", &tagged);
    if let Ok(app) = &tagged {
        let DbEntry::SqlxPg(main) = &app.db["main"] else {
            panic!()
        };
        println!(
            "  port={} max={:?} lazy={}",
            main.connect.port,
            main.pool.as_ref().unwrap().max_connections,
            main.pool.as_ref().unwrap().connect_lazy
        );
    }
}

/// `--set` 的值都是字符串（不经过 try_parsing）
#[test]
fn set_override() {
    let tagged = load::<TaggedApp>(
        BASE,
        &[],
        &[
            ("db.main.connect.port", "5433"),
            ("db.main.pool.connect_lazy", "true"),
        ],
    );
    show("tagged --set port/connect_lazy", &tagged);
    let fixed = load::<FixedApp>(
        "[db.main.connect]\nhost = \"a\"\n",
        &[],
        &[
            ("db.main.connect.port", "5433"),
            ("db.main.pool.connect_lazy", "true"),
        ],
    );
    show("对照组 --set port/connect_lazy", &fixed);
    let pw = load::<TaggedApp>(BASE, &[], &[("db.main.connect.password", "123456")]);
    show("tagged --set password=123456", &pw);
}

/// 只靠环境变量新加一个库（文件里没有这一项）
#[test]
fn env_only_entry() {
    let env = [
        ("HY__DB__EXTRA__KIND", "sqlx-pg"),
        ("HY__DB__EXTRA__CONNECT__HOST", "pg-extra"),
    ];
    let tagged = load::<TaggedApp>(BASE, &env, &[]);
    show("环境变量新加 extra", &tagged);
    if let Ok(app) = &tagged {
        println!("  keys = {:?}", app.db.keys().collect::<Vec<_>>());
    }
}

/// 名字的大小写：config-rs 会不会把 key 转小写
#[test]
fn key_case() {
    let tagged = load::<TaggedApp>(
        "[db.Main]\nkind = \"sqlx-pg\"\n[db.Main.connect]\nhost = \"a\"\n",
        &[],
        &[],
    );
    show("key 大写 Main", &tagged);
    if let Ok(app) = &tagged {
        println!("  keys = {:?}", app.db.keys().collect::<Vec<_>>());
    }
}

// ---- 替代写法 ------------------------------------------------------------------

/// C：种类占一层，`[db.sqlx_pg.main]`。每种一个 map 字段，不经过 serde 的缓冲
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ByKind {
    #[serde(default)]
    sqlx_pg: BTreeMap<String, SqlxPgConfig>,
    #[serde(default)]
    seaorm_pg: BTreeMap<String, SeaOrmPgConfig>,
    #[serde(default)]
    sqlx_sqlite: BTreeMap<String, SqlxSqliteConfig>,
}

#[derive(Debug, Deserialize)]
struct ByKindApp {
    db: ByKind,
}

/// D：外部标签（serde 默认），`[db.main.sqlx_pg]`
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ExtEntry {
    SqlxPg(SqlxPgConfig),
    SeaormPg(SeaOrmPgConfig),
    SqlxSqlite(SqlxSqliteConfig),
}

#[derive(Debug, Deserialize)]
struct ExtApp {
    db: BTreeMap<String, ExtEntry>,
}

const BY_KIND: &str = r#"
[db.sqlx_pg.main.connect]
host = "pg-main"
[db.seaorm_pg.orm.log]
enabled = false
[db.sqlx_sqlite.local.connect]
database = "/tmp/a.db"
"#;

const EXT: &str = r#"
[db.main.sqlx_pg.connect]
host = "pg-main"
[db.orm.seaorm_pg.log]
enabled = false
[db.local.sqlx_sqlite.connect]
database = "/tmp/a.db"
"#;

#[test]
fn alt_by_kind() {
    show("C 基本", &load::<ByKindApp>(BY_KIND, &[], &[]));
    show(
        "C env password=123456",
        &load::<ByKindApp>(
            BY_KIND,
            &[("HY__DB__SQLX_PG__MAIN__CONNECT__PASSWORD", "123456")],
            &[],
        ),
    );
    show(
        "C --set connect_lazy=true port=5433",
        &load::<ByKindApp>(
            BY_KIND,
            &[],
            &[
                ("db.sqlx_pg.main.pool.connect_lazy", "true"),
                ("db.sqlx_pg.main.connect.port", "5433"),
            ],
        ),
    );
    show(
        "C connect 拼错",
        &load::<ByKindApp>(
            &BY_KIND.replace("host = \"pg-main\"", "hots = \"x\""),
            &[],
            &[],
        ),
    );
    show(
        "C 种类拼错",
        &load::<ByKindApp>("[db.sqlx_mysql.x.connect]\nhost = \"a\"\n", &[], &[]),
    );
}

#[test]
fn alt_external() {
    show("D 基本", &load::<ExtApp>(EXT, &[], &[]));
    show(
        "D env password=123456",
        &load::<ExtApp>(
            EXT,
            &[("HY__DB__MAIN__SQLX_PG__CONNECT__PASSWORD", "123456")],
            &[],
        ),
    );
    show(
        "D --set connect_lazy=true port=5433",
        &load::<ExtApp>(
            EXT,
            &[],
            &[
                ("db.main.sqlx_pg.pool.connect_lazy", "true"),
                ("db.main.sqlx_pg.connect.port", "5433"),
            ],
        ),
    );
    show(
        "D connect 拼错",
        &load::<ExtApp>(&EXT.replace("host = \"pg-main\"", "hots = \"x\""), &[], &[]),
    );
    show(
        "D 种类拼错",
        &load::<ExtApp>("[db.x.sqlx_mysql.connect]\nhost = \"a\"\n", &[], &[]),
    );
    show(
        "D 一项写两种",
        &load::<ExtApp>(
            &format!("{EXT}\n[db.main.seaorm_pg.connect]\nhost = \"b\"\n"),
            &[],
            &[],
        ),
    );
}

/// B 再补两条：--set 改数字；环境变量能不能改到文件里大写的名字
#[test]
fn tagged_more() {
    show(
        "B --set port=5433",
        &load::<TaggedApp>(BASE, &[], &[("db.main.connect.port", "5433")]),
    );
    show(
        "B env 改大写名字 Main",
        &load::<TaggedApp>(
            "[db.Main]\nkind = \"sqlx-pg\"\n",
            &[("HY__DB__MAIN__CONNECT__HOST", "x")],
            &[],
        )
        .map(|a| println!("  keys = {:?}", a.db.keys().collect::<Vec<_>>())),
    );
}

// ---- uses 用数组时，环境变量、--set 能不能改 ----------------------------------------

#[derive(Debug, Deserialize)]
struct UsesApp {
    db: BTreeMap<String, UsesEntry>,
}

#[derive(Debug, Deserialize)]
struct UsesEntry {
    uses: Vec<String>,
}

#[test]
fn uses_array_override() {
    let file = "[db.main]\nuses = [\"pool\"]\n";
    let r = load::<UsesApp>(file, &[("HY__DB__MAIN__USES", "pool,lock")], &[]);
    show("env uses=pool,lock", &r);
    if let Ok(a) = &r {
        println!("  {:?}", a.db["main"].uses);
    }
    let r = load::<UsesApp>(file, &[], &[("db.main.uses", "pool,lock")]);
    show("--set uses=pool,lock", &r);
    if let Ok(a) = &r {
        println!("  {:?}", a.db["main"].uses);
    }
    let r = load::<UsesApp>(file, &[], &[("db.main.uses[1]", "lock")]);
    show("--set uses[1]=lock", &r);
    if let Ok(a) = &r {
        println!("  {:?}", a.db["main"].uses);
    }
}
