一份代码同时支持 PostgreSQL、SQLite、MySQL：只用三种库都能对应上、结果一样的类型和写法，提前定好；做不到的换一种存法。

entity（SeaORM 的 Model）只有一份，查询只写一份，换库只改连接串。下面每一条都在三种库上实际跑过
（PostgreSQL 18.6、MySQL 9.7.2、SQLite 3.51.3（sqlx 自带）），没跑过的单独标出。
要用这里没列的类型、函数，先在三种库上各跑一遍（PG、MySQL 用 hygiea-test 的容器，SQLite 用临时文件），再补进来。

三种库里限制最多的是 SQLite：它是动态类型，值只有 `NULL`、`INTEGER`、`REAL`、`TEXT`、`BLOB` 五种存储类，
建表时写的列类型（`DATETIME`、`boolean`、`DECIMAL`）只决定这一列倾向于存成哪一种，没有真正的时间、定点小数类型。
下面做不到的，大多是因为它。

# 1. entity 从 PostgreSQL 生成

```sh
sea-orm-cli generate entity -u postgres://..:..@..:../.. -o src/entity
```

- 生成出来的就是三种库共用的那一份，不用改
- 不要传 `-s`（`--database-schema`）：传了会生成 `schema_name = "public"`，换到别的库就对不上。
  环境变量 `DATABASE_SCHEMA` 也会被当成 `-s`，生成前确认没有设
- 不从 SQLite、MySQL 生成，也不拿三种库各自生成的结果来比：从它们反推出来的有缺陷（MySQL 的 `LONGTEXT` 被标成 `ignore`、
  `tinyint(1)` 推成 `i8`；SQLite 的部分唯一索引被当成单列唯一、`DATETIME` 推成 `String`），只影响生成的代码，不影响运行

# 2. 类型

| 存什么 | entity 里的类型 | 建表（sea-query） | SQLite | PostgreSQL | MySQL |
|---|---|---|---|---|---|
| 主键（自增） | `i64` | `big_integer().auto_increment().primary_key()` | `INTEGER PRIMARY KEY AUTOINCREMENT` | `bigint`（自增） | `bigint AUTO_INCREMENT` |
| 整数 | `i64` | `big_integer()` | `INTEGER` | `bigint` | `bigint` |
| 布尔 | `bool` | `boolean()` | `boolean`（存 0 / 1） | `boolean` | `tinyint(1)` |
| 文本 | `String` | 见 3 | `TEXT` | `text` | `longtext`；在唯一约束、索引里的和有默认值的用 `varchar` |
| 取值固定的几种（状态、种类） | `String`，Rust 里转成枚举 | 同文本 | `TEXT` | `text` | `varchar` |
| 时间 | `i64`，Unix 毫秒（UTC） | `big_integer()` | `INTEGER` | `bigint` | `bigint` |
| JSON | `String`，Rust 里用 serde_json 读写 | 同文本 | `TEXT` | `text` | `longtext` |

不用的类型和换成什么：

| 不用 | 原因 | 换成 |
|---|---|---|
| 各库的时间类型（`timestamptz`、`DATETIME`、`TIMESTAMP`），包括 hygiea 的 [`HyUtcDateTime`](crate::HyUtcDateTime) | SQLite 存成文本：sqlx 写的小数秒长度不固定（整秒时省掉，`…45Z` 和 `…45.5Z`），`CURRENT_TIMESTAMP` 的格式也不一样（`T`、`Z` 和空格），在 SQL 里比较、排序会错，同一秒内、直接改库插入的行都会排错。MySQL 的 `TIMESTAMP` 只到 2038-01-19，`DATETIME` 的默认值按连接的时区取值 | `i64` 存 Unix 毫秒，三种库都按整数比较、排序。Rust 里和 `UtcDateTime` 互转 |
| 定点小数（`decimal`） | SQLite 没有定点小数，`decimal` 列存成浮点数（`REAL`），15～17 位有效数字以外静默丢掉（`0.123456789012345678` 读回来是 `0.1234567890123457`）。另外 Rust 的 `rust_decimal` 本身最多 28 位有效数字 | 金额放大成整数存 `i64`（日元存整数，币存成 10⁻⁸ 这类最小单位，每列的小数位固定）：精确，SQL 里能比较、排序。只存取、不在 SQL 里比较的，也可以存成文本 |
| JSON 类型（`jsonb`、`json`）和 JSON 查询 | 三种库的支持、函数都不一样 | 存成文本，查询在 Rust 里做 |
| 枚举类型（PG 的 enum、MySQL 的 `enum`） | SQLite 没有，三种库写法也不同 | 存成文本，Rust 里转成枚举 |

# 3. 建表

表结构用 sea-orm-migration（sea-query 的构造器）建，三种库通用的写一遍，下面几处按 `manager.get_database_backend()` 分开写。
都用构造器写，不用手写 SQL。

| 哪里不一样 | SQLite、PostgreSQL | MySQL |
|---|---|---|
| 文本列 | `text()` | 在唯一约束、索引里的和有默认值的 `string_len(255)`（`TEXT` 不能直接建索引、不能有默认值），其余 `custom("LONGTEXT")`（`TEXT` 最多 64KB） |
| 名字区分大小写、末尾空格 | 默认就区分 | 表上 `character_set("utf8mb4").collate("utf8mb4_0900_bin")`。不用 `utf8mb4_bin`：它会把末尾的空格忽略掉，`abc` 和 `abc `（带空格）在唯一约束下算重复 |
| 「某一列为空时另一列唯一」 | 部分唯一索引：`Index::create().col(..).unique().and_where(Expr::col(..).is_null())` | 没有部分索引，用函数索引当索引列：`.col(Expr::cust("(IFNULL(environment_id, 0))"))` |
| 时间列的默认值（现在的 Unix 毫秒） | SQLite `Expr::cust("(CAST(unixepoch('subsec') * 1000 AS INTEGER))")`；PostgreSQL `Expr::cust("(floor(extract(epoch from clock_timestamp()) * 1000)::bigint)")` | `Expr::cust("(CAST(UNIX_TIMESTAMP(NOW(3)) * 1000 AS SIGNED))")`，和连接的时区无关 |

- 布尔列的默认值写 `.default(true)` / `.default(false)`，三种库都对
- 直接改库（数据库工具、手写的 SQL）时：布尔写 `TRUE` / `FALSE`（PostgreSQL 的 `boolean` 不收 `1`、`0`）；时间写 Unix 毫秒的整数，
  或者不写、用默认值
- 在数据库工具里看时间：SQLite `strftime('%Y-%m-%d %H:%M:%f', created_at / 1000.0, 'unixepoch')`，
  PostgreSQL `to_timestamp(created_at / 1000.0)`，MySQL `FROM_UNIXTIME(created_at / 1000)`（显示的是连接时区的时间）
- MySQL 的函数索引、表达式默认值、`utf8mb4_0900_bin` 要 MySQL 8.0 以上（具体从 8.0 的哪个小版本开始，按 MySQL 的文档，没有逐个版本试过）
- `key`、`case`、`condition` 这类保留字做列名、表名时，SeaORM 生成的 SQL 自动加引号；手写 SQL 时 PostgreSQL、SQLite 用双引号，MySQL 用反引号

# 4. 查询

查询用 SeaORM 的查询构造写，下面这些三种库是同一份代码、结果一样：

- 按关系一起查（`find_with_related`、`find_also_related`）、按关联表的条件过滤（`join` 加 `filter`）
- 插入后取 id 用插入结果的 `last_insert_id`，不手写 `RETURNING`（MySQL 没有）；`ActiveModel::insert` 直接返回插入后的整行，三种库都能用
- 唯一键查找、`is_null()` 过滤、`Condition::any()` 组合条件
- 排序、`paginate` 分页和计数、`select_only().column(..).into_tuple()` 只取几列
- `ActiveModel` 只改部分字段、`update_many` 按条件批量改、`delete_many` 批量删
- 事务提交、回滚；`begin_with_config(Some(IsolationLevel::RepeatableRead), Some(AccessMode::ReadOnly))` 三种库都不报错
- 整数（包括存成 Unix 毫秒的时间）、布尔在 `WHERE`、`ORDER BY` 里比较

字符串比较三种库的规则不一样，写条件时要按这个来：

| 写法 | SQLite | PostgreSQL | MySQL（列是 `utf8mb4_0900_bin`） |
|---|---|---|---|
| `=` 区分大小写 | 区分 | 区分 | 区分（没指定排序规则的列、字面量之间，按连接默认的 `utf8mb4_0900_ai_ci`，不区分） |
| `LIKE` 区分大小写 | **不区分**（ASCII 字母，在列上也一样） | 区分 | 区分 |
| 末尾空格算不算 | 算 | 算 | 算（`utf8mb4_bin` 不算） |

所以要区分大小写的前缀、模糊匹配，不能靠 `LIKE`（`starts_with`、`contains` 生成的也是 `LIKE`），查出来后在 Rust 里再筛一遍。

# 5. SQL 函数

能不写就不写：长度、大小写转换后的匹配、随机数、时间计算放在 Rust 里做。必须写时只用下表「一样」的那些。

| 函数、写法 | 三种库 |
|---|---|
| `CONCAT`、`LOWER`、`UPPER`、`SUBSTR`、`SUBSTRING`、`TRIM`、`REPLACE`、`COALESCE`、`NULLIF`、`CASE WHEN`、`%`、`ABS` | 一样 |
| `LENGTH('中文')` | SQLite、PostgreSQL 是 2，MySQL 是 6（按字节）。按字符的 `CHAR_LENGTH` SQLite 没有 |
| `IFNULL` | PostgreSQL 没有，用 `COALESCE` |
| `INSTR`、`POSITION(.. IN ..)` | `INSTR` PostgreSQL 没有，`POSITION` SQLite 没有 |
| `GREATEST`、`MAX(a, b)` | `GREATEST` SQLite 没有，多参数的 `MAX` 只有 SQLite 有 |
| `7 / 2` | SQLite、PostgreSQL 是 3，MySQL 是 3.5000 |
| `CAST(.. AS BIGINT)`、`CAST(.. AS SIGNED)` | `BIGINT` MySQL 不认，`SIGNED` PostgreSQL 不认 |
| `RANDOM()`、`RAND()` | 各只有一边认 |
| `CURRENT_TIMESTAMP` | 三种库的类型、格式都不同；要「现在」用 3 里的 Unix 毫秒表达式 |
| `\|\|` 拼接 | sqlx 连 MySQL 时会设 `PIPES_AS_CONCAT`，所以经 sqlx 是拼接；数据库工具直接连 MySQL 时是「或」。用 `CONCAT` |

没测过的不列：聚合（`GROUP BY`、`GROUP_CONCAT` / `string_agg`）、窗口函数、upsert（`on_conflict` 在三种库上生成的语句不同）、全文搜索。
