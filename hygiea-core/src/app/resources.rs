//! 组件之间共享的资源容器

use std::any::{Any, TypeId};
use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

use dashmap::DashMap;

use super::{BaseAppErr, Name};
use crate::{HyErr, err};

#[derive(Hash, PartialEq, Eq)]
struct ResourceKey {
    type_id: TypeId,
    name: Name,
}

/// 一个资源的标识：类型 + 名字（匿名是 `""`），和 [`Resources`] 里存取用的是同一个 key。
///
/// 组件用它声明依赖关系（[`Component::provides`](super::Component::provides) /
/// [`Component::depends_on`](super::Component::depends_on)），Registry 据此排启动顺序：
///
/// ```ignore
/// fn depends_on(&self) -> Vec<ResourceId> {
///     vec![ResourceId::named::<DbPool>("primary"), ResourceId::of::<RedisClient>()]
/// }
/// ```
#[derive(Clone, Debug)]
pub struct ResourceId {
    type_id: TypeId,
    type_name: &'static str,
    name: Name,
}

impl ResourceId {
    /// 匿名资源（`insert` / `get` 存取的那种）
    pub fn of<T: Resource>() -> Self {
        Self::named::<T>("")
    }

    /// 具名资源（`insert_named` / `get_named` 存取的那种）
    pub fn named<T: Resource>(name: impl Into<Name>) -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            type_name: std::any::type_name::<T>(),
            name: name.into(),
        }
    }

    fn key(&self) -> ResourceKey {
        ResourceKey {
            type_id: self.type_id,
            name: self.name.clone(),
        }
    }
}

/// 按类型和名字判断是不是同一个资源，`type_name` 只是给人看的
impl PartialEq for ResourceId {
    fn eq(&self, other: &Self) -> bool {
        self.type_id == other.type_id && self.name == other.name
    }
}

impl Eq for ResourceId {}

impl std::hash::Hash for ResourceId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.type_id.hash(state);
        self.name.hash(state);
    }
}

/// `TypeName(name)`，报错和日志里用
impl fmt::Display for ResourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}({})", self.type_name, self.name)
    }
}

/// 能放进 [`Resources`] 的类型。取出来时返回克隆，所以要 `Clone`；连接池这类类型内部是 `Arc`，克隆很便宜。
/// 满足约束的类型自动实现，不用手写
pub trait Resource: Send + Sync + Clone + 'static {}

impl<T: Send + Sync + Clone + 'static> Resource for T {}

/// 所有组件共享的资源容器。
///
/// 组件启动时按类型读写资源。内部做了类型擦除，对外是带类型的 API。
///
/// 同一个类型可以有匿名和具名两种资源，同类型的多个实例（比如两个 PgPool）可以同时存在。
/// 同一个类型、同一个名字只能插入一次，重复插入会 panic（基本都是写错了，比如两个组件用了同一个名字）。
///
/// # 示例
/// ```ignore
/// // 匿名：每个类型一个
/// resources.insert(RedisClient::new(config));
/// let redis = resources.get::<RedisClient>().unwrap();
///
/// // 具名：同一类型多个实例
/// resources.insert_named("primary", PgPool::new(primary_config));
/// resources.insert_named("replica", PgPool::new(replica_config));
/// let primary = resources.get_named::<PgPool>("primary").unwrap();
/// let replica  = resources.get_named::<PgPool>("replica").unwrap();
/// ```
#[derive(Clone)]
pub struct Resources {
    map: Arc<DashMap<ResourceKey, Arc<dyn Any + Send + Sync>>>,
}

impl Resources {
    pub(super) fn new() -> Self {
        Self {
            map: Arc::new(DashMap::new()),
        }
    }

    fn key<T: Resource>(name: Name) -> ResourceKey {
        ResourceKey {
            type_id: TypeId::of::<T>(),
            name,
        }
    }

    /// 查找用的 key：名字借来的，这里拷一份
    fn lookup_key<T: Resource>(name: &str) -> ResourceKey {
        Self::key::<T>(Cow::Owned(name.to_owned()))
    }

    /// 插入匿名资源。同类型的匿名资源已经有了会 panic
    pub fn insert<T: Resource>(&self, value: T) {
        self.insert_named("", value);
    }

    /// 插入具名资源。同类型、同名的资源已经有了会 panic
    pub fn insert_named<T: Resource>(&self, name: impl Into<Name>, value: T) {
        match self.map.entry(Self::key::<T>(name.into())) {
            dashmap::Entry::Occupied(entry) => panic!(
                "resource {}({}) already exists",
                std::any::type_name::<T>(),
                entry.key().name
            ),
            dashmap::Entry::Vacant(entry) => {
                entry.insert(Arc::new(value));
            }
        }
    }

    /// 取匿名资源（返回克隆）
    pub fn get<T: Resource>(&self) -> Option<T> {
        self.get_named("")
    }

    /// 取具名资源（返回克隆）
    pub fn get_named<T: Resource>(&self, name: &str) -> Option<T> {
        self.map
            .get(&Self::lookup_key::<T>(name))
            .and_then(|entry| (**entry).downcast_ref::<T>().cloned())
    }

    /// 取匿名资源，没有时返回 [`BaseAppErr::ResourceMissing`]。组件在 `startup` 里取依赖用这个，
    /// 缺了就是启动失败，不要 `get().unwrap()`
    pub fn require<T: Resource>(&self) -> Result<T, HyErr> {
        self.require_named("")
    }

    /// 取具名资源，没有时返回 [`BaseAppErr::ResourceMissing`]，错误里写明缺的是哪个类型、哪个名字
    pub fn require_named<T: Resource>(&self, name: &str) -> Result<T, HyErr> {
        self.get_named(name).ok_or_else(|| {
            err!(
                BaseAppErr::ResourceMissing,
                ResourceId::named::<T>(name.to_owned()).to_string()
            )
        })
    }

    /// 这个资源在不在，Registry 校验组件声明的 `provides` 用
    pub(super) fn contains_id(&self, id: &ResourceId) -> bool {
        self.map.contains_key(&id.key())
    }

    /// 是否有这个类型的匿名资源
    pub fn contains<T: Resource>(&self) -> bool {
        self.contains_named::<T>("")
    }

    /// 是否有这个类型、这个名字的具名资源
    pub fn contains_named<T: Resource>(&self, name: &str) -> bool {
        self.map.contains_key(&Self::lookup_key::<T>(name))
    }

    /// 移除匿名资源，和 `get` 一样返回值本身
    pub fn remove<T: Resource>(&self) -> Option<T> {
        self.remove_named("")
    }

    /// 移除具名资源，和 `get_named` 一样返回值本身：没有别处持有时直接取出，否则返回克隆
    pub fn remove_named<T: Resource>(&self, name: &str) -> Option<T> {
        self.map
            .remove(&Self::lookup_key::<T>(name))
            .and_then(|(_, value)| value.downcast::<T>().ok())
            .map(Arc::unwrap_or_clone)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 注意：这里不建 Registry（会装全局日志，占用 log.rs 的 INSTALLED 标记），
    // Resources::new() 是 pub(crate)，本文件在 core 内部可以直接调用

    #[test]
    fn anonymous_and_named_resources_are_independent() {
        let resources = Resources::new();
        resources.insert(1i32);
        resources.insert_named("a", 2i32);

        assert_eq!(resources.get::<i32>(), Some(1));
        assert_eq!(resources.get_named::<i32>("a"), Some(2));
        // 匿名的 get_named("") 和 get 是同一个资源
        assert_eq!(resources.get_named::<i32>(""), Some(1));
    }

    #[test]
    fn different_types_same_name_do_not_interfere() {
        let resources = Resources::new();
        resources.insert_named::<i32>("x", 1);
        resources.insert_named::<String>("x", "hello".to_string());

        assert_eq!(resources.get_named::<i32>("x"), Some(1));
        assert_eq!(
            resources.get_named::<String>("x"),
            Some("hello".to_string())
        );
    }

    #[test]
    #[should_panic(expected = "already exists")]
    fn insert_duplicate_anonymous_panics() {
        let resources = Resources::new();
        resources.insert(1i32);
        resources.insert(2i32);
    }

    #[test]
    #[should_panic(expected = "already exists")]
    fn insert_named_duplicate_panics() {
        let resources = Resources::new();
        resources.insert_named("a", 1i32);
        resources.insert_named("a", 2i32);
    }

    #[test]
    fn require_missing_returns_resource_missing_with_type_and_name() {
        let resources = Resources::new();
        let err = resources.require_named::<i32>("primary").unwrap_err();
        assert!(err.is(BaseAppErr::ResourceMissing));
        let msg = err.to_string();
        assert!(msg.contains("i32"), "{msg}");
        assert!(msg.contains("primary"), "{msg}");
    }

    #[test]
    fn require_anonymous_missing_returns_resource_missing() {
        let resources = Resources::new();
        let err = resources.require::<i32>().unwrap_err();
        assert!(err.is(BaseAppErr::ResourceMissing));
    }

    #[test]
    fn remove_then_contains_is_false() {
        let resources = Resources::new();
        resources.insert_named("a", 1i32);
        assert!(resources.contains_named::<i32>("a"));

        let removed = resources.remove_named::<i32>("a");
        assert_eq!(removed, Some(1));
        assert!(!resources.contains_named::<i32>("a"));
    }

    #[test]
    fn resource_id_of_equals_named_with_empty_name() {
        assert_eq!(ResourceId::of::<i32>(), ResourceId::named::<i32>(""));
    }

    #[test]
    fn resource_id_hash_matches_eq() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        fn hash_of(id: &ResourceId) -> u64 {
            let mut hasher = DefaultHasher::new();
            id.hash(&mut hasher);
            hasher.finish()
        }

        let a = ResourceId::named::<i32>("x");
        let b = ResourceId::named::<i32>("x");
        assert_eq!(a, b);
        assert_eq!(hash_of(&a), hash_of(&b));

        // 类型不同，即使名字相同也不该相等（也顺带验证 type_name 不参与 eq 本身不影响这里）
        let c = ResourceId::named::<i64>("x");
        assert_ne!(a, c);
    }

    #[test]
    fn resource_id_display_format() {
        let id = ResourceId::named::<i32>("primary");
        assert_eq!(id.to_string(), "i32(primary)");

        let anon = ResourceId::of::<i32>();
        assert_eq!(anon.to_string(), "i32()");
    }
}
