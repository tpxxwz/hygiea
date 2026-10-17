# live config（数据库配置表）：内部流程

怎么用、配置、读法、表结构、出了问题会怎样（对使用方而言）、用的时候要注意，都在模块文档里：
`hygiea-components/hygiea-db/src/live_config/mod.rs`（docs.rs 上是 `hygiea::db::live_config`），各类型、方法的细节在它们自己的文档。
这里只讲内部怎么做、为什么这样做。代码在 `hygiea-components/hygiea-db/src/live_config/`，
跑起来看效果：`cargo run -p hygiea-examples --example db_live_config`。

下面的例子里应用名是 `dict_jp`、没配 `table_prefix`，所以表名是 `dict_jp_config` 这一套，schema 是 `dict_jp`。

---

## 1. 由哪些东西组成

```mermaid
flowchart LR
    subgraph app["应用"]
        biz["业务代码<br/>runtime.get()"]
    end
    subgraph pool["连接池组件（SqlxPgComponent 等）"]
        main["业务池<br/>uses 带 1 时有"]
        src["DbLiveConfig（LiveConfigSource）<br/>uses 带 2 时有，按组件名放进 Resources"]
    end
    subgraph lc["live_config"]
        worker["Worker<br/>后台任务"]
        store["Store<br/>独立小池（1 条连接）<br/>+ 查询 + 收通知"]
        handle["LiveConfig&lt;T&gt;<br/>watch 接收端"]
    end
    db[("数据库<br/>dict_jp_config<br/>dict_jp_config_revision<br/>dict_jp_config_history<br/>+ 触发器")]

    biz --> handle
    src -- "第一次 bind 时<br/>把 Store 交给" --> worker
    worker --> store
    store <--> db
    worker -- "send_replace 新值" --> handle
    main <--> db
```

各部分在哪个文件见 `tools/docs.py tree hygiea-components/hygiea-db/src/live_config`：和驱动无关的（源、句柄、worker、表的规则）
在 `live_config/` 下，SQL 在 `src/sql/<数据库>/live_config.*`，执行在 `live_config/sqlx/`（一种数据库一个文件）。

---

## 2. 启动：组件建资源的时候

这一步不连库、不建表，只看配置。

```mermaid
flowchart TD
    A["组件启动（DbResources::from_config）"] --> B{"uses 带 2？"}
    B -- 否 --> C["不放 DbLiveConfig<br/>写了 [db.live_config] 段的启动报错"]
    B -- 是 --> D["表名前缀 = table_prefix（不写用应用名）<br/>校验前缀、poll_interval_secs > 0"]
    D -- 不合法 --> E["报错，启动失败"]
    D -- 合法 --> F["建 Store：独立小池（lazy，不连库）<br/>连接参数 = [db.live_config.connect] 盖到 [db.connect] 上<br/>拼好 SQL"]
    F --> G["DbLiveConfig 先保管着 Store，按组件名放进 Resources"]
```

---

## 3. 第一次 bind：连库、建表、起后台任务

```mermaid
sequenceDiagram
    participant App as 应用（before_activate）
    participant Src as DbLiveConfig
    participant St as Store
    participant DB as 数据库
    participant W as Worker（后台任务）

    App->>Src: bind_all(RuntimeConfig::default())
    Src->>Src: 默认值转成 JSON 对象<br/>{markji_sync: {..}, moji_cache: {..}}
    Note over Src: 第一次 bind 才做下面这段（只做一次）
    Src->>St: open(检查表)
    St->>DB: 小池第一次连上（PG 建 PgListener）
    St->>DB: PG：查 current_schema()，channel = <schema>.<配置表名><br/>（超过 63 字节拒绝启动）
    St->>DB: 查 current_schema 里三张表的列、函数、触发器
    alt 一个都没有
        St->>DB: 加锁（PG advisory lock (builtin::LOCK_TYPE, Lock::LiveConfigDdl) / MySQL GET_LOCK / SQLite BEGIN IMMEDIATE）<br/>再查一遍，还是没有就执行 live_config.sql
    else 都在且列一致
        St->>DB: PG：再看 config_record 函数通知的 channel<br/>对不上（旧版建的、schema 改过名）也报 LiveConfigSchemaMismatch
        Note over St: 直接用
    else 只有一部分 / 列对不上
        St-->>App: LiveConfigSchemaMismatch（日志里有哪里不对和 DDL）
    end
    St->>DB: PG：LISTEN dict_jp_config
    Src->>W: 起后台任务，Store 交给它
    Src->>W: Bind 命令（默认值 + 绑定）
    loop 每个 namespace 的每个 key
        W->>DB: INSERT ... ON CONFLICT DO NOTHING<br/>（MySQL 是 ON DUPLICATE KEY UPDATE 空操作）<br/>（表里没有才补默认值）
    end
    loop 每个 namespace
        W->>DB: 一条语句读出 revision + 所有行
    end
    W->>W: 默认值打底，表里的值覆盖上去<br/>转成 RuntimeConfig
    alt 转换成功
        W-->>Src: Ok
        Src-->>App: LiveConfig（RuntimeConfig）
    else 转不了（类型不对）
        W-->>App: LiveConfigDecodeFailed，启动失败
    end
```

后面再调 `bind` / `bind_all`，就不用再连库、建表了，直接发 Bind 命令给已经在跑的后台任务，同一个后台任务、同一条连接。

---

## 4. 运行中改了表：怎么传到程序

### 数据库那一侧（触发器）

```mermaid
flowchart TD
    U["UPDATE dict_jp_config SET value = .. <br/>（任何程序、任何连接）"] --> T{"触发器判断"}
    T -- 值没变 --> N["什么都不做"]
    T -- 只改了值 --> R1["记 U"]
    T -- 改了 namespace 或 key --> R2["旧的记 D，新的记 I"]
    T -- INSERT / DELETE --> R3["记 I / D"]
    R1 --> REC
    R2 --> REC
    R3 --> REC
    REC["dict_jp_config_record()"] --> V["dict_jp_config_revision 这一行 revision + 1<br/>（行锁：同 namespace 并发改，版本号按提交顺序递增）"]
    V --> H["dict_jp_config_history 追加一行<br/>旧值、新值、revision、时间、账号"]
    H --> P{"PG?"}
    P -- 是 --> NT["pg_notify('dict_jp.dict_jp_config', 'markji_sync:4')<br/>channel 带 schema 名，建表时写进函数<br/>事务提交后才真正发出，回滚不发"]
    P -- MySQL / SQLite --> X["没有通知，等程序轮询"]
```

### 程序这一侧（worker）

```mermaid
sequenceDiagram
    participant DB as 数据库
    participant W as Worker
    participant H as LiveConfig（各处的 clone）
    participant Biz as 业务代码

    alt PG
        DB-->>W: 通知 "markji_sync:4"
        W->>W: 4 比本地的 3 大？大才往下
    else MySQL / SQLite（或 PG 每 60 秒兜底）
        W->>DB: SELECT namespace, revision FROM dict_jp_config_revision
        W->>W: 找出版本号变大的 namespace
    end
    W->>DB: 重新读这个 namespace（revision + 所有行，同一个快照）
    W->>W: 记下新版本号和值
    W->>W: 用到这个 namespace 的每个绑定：<br/>默认值打底 + 表里的值覆盖，转成 T
    alt 转换成功
        W->>H: send_replace(Arc::new(新值))
        H-->>Biz: get() 拿到新值；changed() 返回
    else 转不了
        W->>W: 打 error 日志，保留旧值
    end
```

`bind_all` 的时候，一个 namespace 变了只重新读它自己，其他 namespace 用内存里的，整体重新转一个新的 `RuntimeConfig`。转换失败的话，整个 `RuntimeConfig` 保留旧值，不会出现一半新一半旧。

---

## 5. Worker 主循环

worker 是一个 tokio 任务，所有数据库操作都在它手里、在一条连接上串行执行。

```mermaid
flowchart TD
    L(["循环"]) --> O{"Store 能用？<br/>（PG 的 listener 建好了）"}
    O -- 否 --> RB["在同一个小池上重建 listener + LISTEN<br/>成功：对一遍版本号<br/>失败：等 5 秒"]
    RB --> L
    O -- 是 --> S["select! 同时等四件事，先到先处理"]
    S --> E1["退出信号"] --> END["关小池，结束"]
    S --> E2["Bind 命令"] --> B["补默认值、加载、回复"] --> L
    S --> E3["定时器到点"] --> SY["对一遍版本号，读变大的"] --> L
    S --> E4["store.recv()"]
    E4 --> N1["通知 namespace:revision"] --> RL["版本更大就重新读"] --> L
    E4 --> N2["sqlx 断线又自动重连了"] --> SY
    E4 --> N3["报错"] --> F["PG：丢掉 listener（下一轮重建）<br/>SQLite：只打日志"] --> L
```

几点说明：

- 业务读和 worker 写之间是 tokio `watch` 里的一把读写锁：`get()` 加读锁、clone 一下 `Arc`（引用计数 +1）就放开；
  worker 加写锁、把 `Arc` 换成新值就放开（转 struct 在加锁之前做完）。只有配置真的变了才写，读几乎不会等。
  拿到的 `Arc<T>` 是快照，之后换了新值也不影响手上这份；旧值等最后一个人用完才释放

- 等通知的那一步随时可以被取消：定时器到了、来了 Bind 命令，等通知的 future 会被丢掉，处理完再接着等。sqlx 收消息时读完整条才动缓冲区，丢掉也不会把连接弄坏
- 查询的时候来了通知，sqlx 先放进缓冲区，查完照样能取到，不会丢
- 只有读到的版本比本地大才换值：重复的通知、多实例、乱序都不会把新值换回旧的

---

## 6. 出了问题会怎样

| 情况 | 谁处理 | 结果 |
|---|---|---|
| 网络闪断（EOF、超时等） | sqlx 自动重连、重新 LISTEN | worker 收到"断过一次"，对一遍版本号补上漏掉的 |
| sqlx 返回别的错误（connection reset 等） | worker 丢掉 listener，在同一个小池上重建 | 重建后对一遍版本号 |
| 库挂了 | 重建失败，每 5 秒再试 | 这期间 `get()` 一直是旧值，不报错 |
| 连接被 NAT、防火墙悄悄断掉（不报错、也收不到通知） | PG 每 60 秒兜底轮询 | 轮询报错就重建，或者发现版本变大就补上 |
| 有人把值改成转不了的类型 | worker | 保留旧值，打 error；改回能转的值后恢复 |
| 有人删了 key | worker | 退回代码当前的默认值；下次 bind 补回表里 |

---

## 7. 关闭

```mermaid
sequenceDiagram
    participant R as Registry
    participant P as 连接池组件
    participant Src as DbLiveConfig
    participant W as Worker

    R->>R: 先关 HTTP / gRPC（处理完手上的请求，这期间配置照常更新）
    R->>P: stop()（DbResources::close）
    P->>Src: close()（先关动态配置）
    Src->>W: 取消信号
    W->>W: 退出循环，放掉 listener，关小池
    Src->>Src: 等 worker 结束（没 bind 过的话，这里关小池）
    P->>P: 再关锁单独的池、业务池
    Note over W: 之后 get() 还是最后的值，<br/>changed() / 再 bind 报 LiveConfigClosed，<br/>on_change 的任务退出
```

---

## 8. 为什么这样做

- **字段方法每次读最新**：思路和 .NET 的 `IOptionsMonitor<T>.CurrentValue`、Netflix Archaius 的 `Property<T>.get()` 一样，
  持有的是句柄不是值。代价是连续调几个字段方法时中间可能换了版本，所以另外留了整块快照（`get()` / `snapshot()`）
- **版本号每个 namespace 一个、只接受更大的**：变了只重新读那一个 namespace；重复的通知、多实例、乱序都不会把新值换回旧的
- **变更历史程序不读**：版本号单独放在 `_config_revision`，历史只给人查，随便删不影响生效的值
- **表结构不迁移**：要么全对上直接用，要么一个都没有全建，有一个对不上就报错不启动，以后改结构手动执行 DDL
