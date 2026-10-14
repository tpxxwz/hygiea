//! 连接池组件：一份配置按 `uses` 建出业务池、动态配置、锁，放进 Resources。6 种连接池（sqlx / SeaORM × PG / MySQL / SQLite）
//! 共用这一份：解析 `uses`、检查配置段、建的顺序、放进 Resources、关的顺序都在这里，
//! 各连接池只实现 [`DbConfig`]（怎么建业务池、怎么建出锁和动态配置）

use serde::Deserialize;
use tokio::task::JoinHandle;

use hygiea_core::app::{
    AppErr, CancellationToken, ImmediateComponent, Name, RegistryConfig, Resource, ResourceId,
    Resources, async_trait, component,
};
use hygiea_core::{Result, err};

#[cfg(live_config_any)]
use crate::live_config::DbLiveConfig;
#[cfg(lock_any)]
use crate::lock::DbLock;

// ---- uses -------------------------------------------------------------------

/// 这个组件提供什么，配置里写一个数字，按位加：1 业务池、2 动态配置、4 锁（`uses = 7` 是全部），不写是 1，只有业务池。
///
/// | 位 | 放进 Resources 的资源 |
/// |---|---|
/// | 1 pool | 连接池（比如 `SqlxPgPool`），按组件名 |
/// | 2 live_config | `DbLiveConfig`，按组件名。几个组件都开就是几个源，各管各的 namespace |
/// | 4 lock | `DbLock`，匿名：整个应用只能有一个组件开，两个都开时启动报 `DuplicateProvider` |
///
/// - 0、大于 7、没编译进来的功能（没开 feature `live-config` 却带了 2，没开 `lock` 却带了 4）解析配置时报错；
///   SQLite 带了 4（没有锁）启动时报错
/// - 没带的位对应的段写了，启动报错，免得以为开了其实没开：没带 1 时 `pool`（SeaORM 还有 `log`），没带 2 时 `live_config`，
///   没带 4 时 `lock`
/// - 没带 1（没有业务池）时，锁、动态配置连 `connect`，池参数用业务池的默认值，`live_config.connect`、`lock.connect`
///   覆盖段也不能写（没有业务池可覆盖，直接写在 `connect` 里）。锁的池大小可以在 `lock.max_connections` 改
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "u8")]
pub struct DbUses(u8);

impl DbUses {
    /// 业务池
    pub const POOL: u8 = 1;
    /// 动态配置
    pub const LIVE_CONFIG: u8 = 2;
    /// 锁
    pub const LOCK: u8 = 4;

    pub fn bits(self) -> u8 {
        self.0
    }

    pub fn pool(self) -> bool {
        self.0 & Self::POOL != 0
    }

    pub fn live_config(self) -> bool {
        self.0 & Self::LIVE_CONFIG != 0
    }

    pub fn lock(self) -> bool {
        self.0 & Self::LOCK != 0
    }
}

impl Default for DbUses {
    fn default() -> Self {
        Self(Self::POOL)
    }
}

impl TryFrom<u8> for DbUses {
    type Error = hygiea_core::HyErr;

    /// 不合法的报 `AppErr::InvalidConfig`，规则见 [`DbUses`]
    fn try_from(bits: u8) -> Result<Self> {
        let invalid = |cause: String| Err(err!(AppErr::InvalidConfig, cause));
        if bits == 0 || bits > 7 {
            return invalid(format!(
                "uses = {bits}: must be 1..=7 (1 pool + 2 live_config + 4 lock)"
            ));
        }
        if bits & Self::LIVE_CONFIG != 0 && !cfg!(live_config_any) {
            return invalid(format!(
                "uses = {bits}: bit 2 (live_config) needs feature live-config"
            ));
        }
        if bits & Self::LOCK != 0 && !cfg!(lock_any) {
            return invalid(format!(
                "uses = {bits}: bit 4 (lock) needs feature lock (postgres or mysql)"
            ));
        }
        Ok(Self(bits))
    }
}

// ---- 各连接池实现的部分 ------------------------------------------------------------

/// 配置里写了哪些段，[`DbConfig::sections`] 给，建之前按 `uses` 检查（规则见 [`DbUses`]）
#[derive(Default)]
pub struct Sections {
    pub(crate) uses: DbUses,
    /// 写了的、只给业务池用的段名：`pool`，SeaORM 还有 `log`
    pub(crate) pool: Vec<&'static str>,
    pub(crate) live_config: bool,
    /// `live_config.connect` 写了至少一项
    pub(crate) live_config_connect: bool,
    pub(crate) lock: bool,
    /// `lock.connect` 写了至少一项
    pub(crate) lock_connect: bool,
}

/// 连接池的配置（`SqlxPgConfig` 等）实现它，[`DbComponent`]、[`DbResources`] 经它建资源。只在本 crate 里实现，
/// 导出是为了写泛型代码（比如测试里对几种连接池共用的辅助函数）
#[async_trait]
pub trait DbConfig: Clone + Send + Sync + 'static {
    /// 业务池
    type Pool: Resource;
    /// 这个数据库有没有锁（SQLite 没有）
    const LOCK: bool;

    fn sections(&self) -> Sections;

    /// 建业务池，`uses` 带 1 时调
    async fn connect(&self, registry_config: &RegistryConfig) -> Result<Self::Pool>;

    /// 关业务池：等在途查询跑完后关掉
    async fn close(pool: &Self::Pool) -> Result<()>;

    /// 建动态配置，`uses` 带 2 时调。只校验配置、不连库，第一次绑定时才连
    #[cfg(live_config_any)]
    fn live_config(&self, registry_config: &RegistryConfig) -> Result<DbLiveConfig>;

    /// 建锁，`uses` 带 4 时调。`pool` 是业务池（`uses` 带 1 时有），锁的段没写覆盖时和它共用连接。
    /// 没有锁的数据库不实现，检查配置时已经拦下了
    #[cfg(lock_any)]
    async fn lock(
        &self,
        pool: Option<&Self::Pool>,
        registry_config: &RegistryConfig,
    ) -> Result<DbLock> {
        let _ = (pool, registry_config);
        Err(err!(AppErr::InvalidConfig, "this database has no lock"))
    }
}

/// 按 `uses` 检查写了的段，规则见 [`DbUses`]
fn check<C: DbConfig>(sections: &Sections) -> Result<()> {
    let uses = sections.uses;
    let bits = uses.bits();
    let invalid = |cause: String| Err(err!(AppErr::InvalidConfig, cause));
    if !uses.pool() {
        if let Some(section) = sections.pool.first() {
            return invalid(format!(
                "uses = {bits} has no pool (1): [{section}] is only for the pool, remove it or add 1"
            ));
        }
        for (set, section) in [
            (sections.live_config_connect, "live_config.connect"),
            (sections.lock_connect, "lock.connect"),
        ] {
            if set {
                return invalid(format!(
                    "uses = {bits} has no pool (1): [{section}] overrides the pool's connect, write the settings in [connect]"
                ));
            }
        }
    }
    if !uses.live_config() && sections.live_config {
        return invalid(format!(
            "uses = {bits} has no live_config (2): remove [live_config] or add 2"
        ));
    }
    if !uses.lock() && sections.lock {
        return invalid(format!(
            "uses = {bits} has no lock (4): remove [lock] or add 4"
        ));
    }
    if uses.lock() && !C::LOCK {
        return invalid(format!(
            "uses = {bits}: this database has no lock, remove 4"
        ));
    }
    Ok(())
}

// ---- 资源 ---------------------------------------------------------------------

/// 一个组件按 `uses` 建出来的资源，没开的是 `None`。经过 Registry 时由 [`DbComponent`] 建好放进 Resources；
/// 不经过 Registry（测试、脚本）直接用 [`from_config`](Self::from_config)，用完 [`close`](Self::close)
pub struct DbResources<C: DbConfig> {
    /// 业务池，`uses` 带 1
    pub pool: Option<C::Pool>,
    /// 动态配置，`uses` 带 2
    #[cfg(live_config_any)]
    pub live_config: Option<DbLiveConfig>,
    /// 锁，`uses` 带 4
    #[cfg(lock_any)]
    pub lock: Option<DbLock>,
}

impl<C: DbConfig> Clone for DbResources<C> {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
            #[cfg(live_config_any)]
            live_config: self.live_config.clone(),
            #[cfg(lock_any)]
            lock: self.lock.clone(),
        }
    }
}

impl<C: DbConfig> std::fmt::Debug for DbResources<C> {
    /// 只打建了哪些（连接池里有连接参数，不打）
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = f.debug_struct("DbResources");
        s.field("pool", &self.pool.is_some());
        #[cfg(live_config_any)]
        s.field("live_config", &self.live_config.is_some());
        #[cfg(lock_any)]
        s.field("lock", &self.lock.is_some());
        s.finish()
    }
}

impl<C: DbConfig> DbResources<C> {
    /// 先检查配置（见 [`DbUses`]），再按动态配置（不连库）、业务池、锁的顺序建。中途失败时把建好的关掉
    pub async fn from_config(config: &C, registry_config: &RegistryConfig) -> Result<Self> {
        check::<C>(&config.sections())?;
        let mut resources = Self {
            pool: None,
            #[cfg(live_config_any)]
            live_config: None,
            #[cfg(lock_any)]
            lock: None,
        };
        if let Err(e) = resources.open(config, registry_config).await {
            if let Err(close) = resources.close().await {
                tracing::warn!("close after failed start: {close:#}");
            }
            return Err(e);
        }
        Ok(resources)
    }

    async fn open(&mut self, config: &C, registry_config: &RegistryConfig) -> Result<()> {
        let uses = config.sections().uses;
        #[cfg(live_config_any)]
        if uses.live_config() {
            self.live_config = Some(config.live_config(registry_config)?);
        }
        if uses.pool() {
            self.pool = Some(config.connect(registry_config).await?);
        }
        #[cfg(lock_any)]
        if uses.lock() {
            self.lock = Some(config.lock(self.pool.as_ref(), registry_config).await?);
        }
        Ok(())
    }

    /// 动态配置的后台任务退出、锁单独建的池关掉（等持有中的锁还回来），最后关业务池（等在途查询跑完）
    pub async fn close(&self) -> Result<()> {
        #[cfg(live_config_any)]
        if let Some(live_config) = &self.live_config {
            live_config.close().await;
        }
        #[cfg(lock_any)]
        if let Some(lock) = &self.lock {
            lock.close().await;
        }
        if let Some(pool) = &self.pool {
            C::close(pool).await?;
        }
        Ok(())
    }
}

// ---- 组件 ---------------------------------------------------------------------

/// 连接池组件：按配置的 `uses`（见 [`DbUses`]）把业务池、`DbLiveConfig` 按组件名，`DbLock` 匿名放进 Resources；
/// 停止时按 [`DbResources::close`] 的顺序关掉。各连接池的组件是它的别名，比如
/// `pub type SqlxPgComponent = DbComponent<SqlxPgConfig>;`
///
/// 同一个组件可以用不同的名字注册多次（主库、从库、单独的协调库），资源按组件名取：
///
/// ```ignore
/// let pool = resources.require_named::<SqlxPgPool>("main")?;
/// let sync = resources.require_named::<DbLiveConfig>("main")?.bind("markji_sync", MarkjiSync::default()).await?;
/// let lock = resources.require::<DbLock>()?;
/// ```
pub struct DbComponent<C: DbConfig> {
    name: Name,
    config: C,
    /// 启动后留一份，关闭时在 `stop` 里关掉
    resources: Option<DbResources<C>>,
}

#[component]
impl<C: DbConfig> ImmediateComponent for DbComponent<C> {
    type Config = C;

    fn build(name: Name, config: C) -> Self {
        Self {
            name,
            config,
            resources: None,
        }
    }

    fn provides(&self) -> Vec<ResourceId> {
        let uses = self.config.sections().uses;
        let mut provides = Vec::new();
        if uses.pool() {
            provides.push(ResourceId::named::<C::Pool>(self.name.clone()));
        }
        #[cfg(live_config_any)]
        if uses.live_config() {
            provides.push(ResourceId::named::<DbLiveConfig>(self.name.clone()));
        }
        #[cfg(lock_any)]
        if uses.lock() {
            provides.push(ResourceId::of::<DbLock>());
        }
        provides
    }

    async fn startup(
        &mut self,
        resources: &Resources,
        _shutdown: CancellationToken,
    ) -> Result<Option<JoinHandle<()>>> {
        let built = DbResources::from_config(&self.config, resources.registry_config()).await?;
        if let Some(pool) = &built.pool {
            resources.insert_named(self.name.clone(), pool.clone());
        }
        #[cfg(live_config_any)]
        if let Some(live_config) = &built.live_config {
            resources.insert_named(self.name.clone(), live_config.clone());
        }
        #[cfg(lock_any)]
        if let Some(lock) = &built.lock {
            resources.insert(lock.clone());
        }
        self.resources = Some(built);
        Ok(None)
    }

    async fn stop(&mut self) -> Result<()> {
        if let Some(resources) = self.resources.take() {
            resources.close().await?;
        }
        Ok(())
    }
}

/// 各连接池单元测试共用：默认（`uses = 1`）按组件名提供业务池，匿名的名字是 `""`；编译进来的位全开时再加上
/// 动态配置（按组件名）、锁（匿名）。`with_uses` 按给的 `uses` 建配置
#[cfg(test)]
pub(crate) fn check_provides<C: DbConfig>(default: C, with_uses: impl Fn(DbUses) -> C) {
    use hygiea_core::app::Component;

    let named = DbComponent::<C>::build(Name::from("primary"), default.clone());
    assert_eq!(
        named.provides(),
        vec![ResourceId::named::<C::Pool>("primary")]
    );
    let anonymous = DbComponent::<C>::build(Name::from(""), default);
    assert_eq!(anonymous.provides(), vec![ResourceId::of::<C::Pool>()]);

    // 没开 live-config、lock 时下面不加，不用 mut
    #[allow(unused_mut)]
    let mut bits = DbUses::POOL;
    #[allow(unused_mut)]
    let mut expected = vec![ResourceId::named::<C::Pool>("primary")];
    #[cfg(live_config_any)]
    {
        bits |= DbUses::LIVE_CONFIG;
        expected.push(ResourceId::named::<DbLiveConfig>("primary"));
    }
    #[cfg(lock_any)]
    if C::LOCK {
        bits |= DbUses::LOCK;
        expected.push(ResourceId::of::<DbLock>());
    }
    let full = DbComponent::<C>::build(
        Name::from("primary"),
        with_uses(DbUses::try_from(bits).unwrap()),
    );
    assert_eq!(full.provides(), expected);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 只用来测 [`check`]（只看 `LOCK`），写了哪些段直接传给 `check`
    #[derive(Clone)]
    struct Fake;

    #[async_trait]
    impl DbConfig for Fake {
        type Pool = ();
        const LOCK: bool = false;

        fn sections(&self) -> Sections {
            Sections::default()
        }

        async fn connect(&self, _: &RegistryConfig) -> Result<()> {
            Ok(())
        }

        async fn close(_: &()) -> Result<()> {
            Ok(())
        }

        #[cfg(live_config_any)]
        fn live_config(&self, _: &RegistryConfig) -> Result<DbLiveConfig> {
            Err(err!(AppErr::InvalidConfig, "fake"))
        }
    }

    fn checked(sections: Sections) -> std::result::Result<(), String> {
        check::<Fake>(&sections).map_err(|e| e.to_string())
    }

    fn uses(bits: u8) -> DbUses {
        DbUses(bits)
    }

    /// 没带的位对应的段写了报错；没有业务池时覆盖段不能写；没有锁的数据库不能带 4
    #[test]
    fn sections_follow_uses() {
        assert!(checked(Sections::default()).is_ok());
        let e = checked(Sections {
            uses: uses(2),
            pool: vec!["log"],
            ..Default::default()
        })
        .unwrap_err();
        assert!(e.contains("[log] is only for the pool"), "{e}");
        let e = checked(Sections {
            uses: uses(2),
            live_config: true,
            live_config_connect: true,
            ..Default::default()
        })
        .unwrap_err();
        assert!(e.contains("[live_config.connect] overrides"), "{e}");
        // 有业务池时覆盖段可以写
        assert!(
            checked(Sections {
                uses: uses(3),
                live_config: true,
                live_config_connect: true,
                ..Default::default()
            })
            .is_ok()
        );
        let e = checked(Sections {
            uses: uses(1),
            live_config: true,
            ..Default::default()
        })
        .unwrap_err();
        assert!(e.contains("has no live_config (2)"), "{e}");
        let e = checked(Sections {
            uses: uses(3),
            lock: true,
            ..Default::default()
        })
        .unwrap_err();
        assert!(e.contains("has no lock (4)"), "{e}");
        let e = checked(Sections {
            uses: uses(5),
            lock: true,
            ..Default::default()
        })
        .unwrap_err();
        assert!(e.contains("this database has no lock"), "{e}");
    }

    /// 不写是 1；1..=7 都能解析（没编译进来的功能除外），0 和大于 7 报错
    #[test]
    fn uses_parse() {
        #[derive(Deserialize)]
        struct T {
            #[serde(default)]
            uses: DbUses,
        }
        let parse = |s: &str| toml::from_str::<T>(s).map(|t| t.uses);
        assert_eq!(parse("").unwrap(), DbUses(1));
        assert!(parse("").unwrap().pool());
        assert!(parse("uses = 0").is_err());
        assert!(parse("uses = 8").is_err());
        assert!(parse("uses = \"7\"").is_err(), "toml 里要写数字");
        let five = parse("uses = 5");
        if cfg!(lock_any) {
            let five = five.unwrap();
            assert!(five.pool() && five.lock() && !five.live_config());
        } else {
            assert!(five.is_err());
        }
        let two = parse("uses = 2");
        assert_eq!(two.is_ok(), cfg!(live_config_any));
    }
}
