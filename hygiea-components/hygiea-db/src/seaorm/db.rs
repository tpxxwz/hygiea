//! 执行 SeaORM 的语句，失败时把 `DbErr` 转成 [`RepoErr`] 的通用错误，原始的 `DbErr` 挂 source。两种写法：
//!
//! - [`db!`](crate::db)：包住整条调用（含 `.await`），从语句类型知道是哪种操作、哪张表，op 是"表名 at 文件:行号"，
//!   跨行时是 `db!` 开头那一行
//! - [`.db()`](DbResultExt::db)：接在 `.await` 后面，什么写法都能用（原生 SQL、事务、分页器），
//!   只有 [`RepoErr::ExecFailed`] 和调用位置
//!
//! ```ignore
//! use hygiea::db::db;
//! use hygiea::db::seaorm::DbResultExt;
//!
//! let user = db!(user::Entity::find_by_id(id).one(txn))?;  // QueryFailed，op "public.user at src/repo.rs:12"
//! db!(user::Entity::delete_by_id(id).exec(txn))?;          // DeleteFailed
//! db!(am.insert(txn))?;                                    // ActiveModel 的方法也行，InsertFailed
//! txn.execute_unprepared(sql).await.db()?;                 // ExecFailed，op "src/repo.rs:15"
//! ```

use std::fmt::Display;
use std::panic::Location;

use hygiea_core::{HyErr, Result, ResultExt, err};
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, DbErr, DeleteMany, DeleteOne, EntityTrait, Insert,
    InsertMany, IntoActiveModel, Paginator, Select, SelectFive, SelectFour, SelectFourMany,
    SelectSix, SelectThree, SelectThreeMany, SelectTwo, SelectTwoMany, SelectTwoRequired, Selector,
    SelectorRaw, SelectorTrait, Topology, TryInsert, UpdateMany, UpdateOne, ValidatedDeleteOne,
    ValidatedUpdateOne,
};

use crate::RepoErr;

/// 错误模板里的 op：表名，实体带 schema 时是 `schema.table`。只在出错时拼
pub(super) fn table<E: EntityTrait>() -> String {
    let entity = E::default();
    match entity.schema_name() {
        Some(schema) => format!("{schema}.{}", entity.table_name()),
        None => entity.table_name().to_owned(),
    }
}

/// 给 SeaORM 执行方法的返回值加 [`db`](DbResultExt::db)
pub trait DbResultExt<T> {
    /// 失败时转成 [`RepoErr::ExecFailed`]，op 是 `.db()` 所在的文件和行号（调用链跨行时是 `.db()` 那一行）
    fn db(self) -> Result<T>;
}

impl<T> DbResultExt<T> for std::result::Result<T, DbErr> {
    #[track_caller]
    fn db(self) -> Result<T> {
        match self {
            Ok(v) => Ok(v),
            Err(e) => {
                let at = Location::caller();
                let op = format!("{}:{}", at.file(), at.line());
                Err(e).wrap_err(|| err!(RepoErr::ExecFailed, op))
            }
        }
    }
}

/// 包住一条 SeaORM 调用（最后一步是执行方法，不写 `.await`），失败时按语句类型转成 [`RepoErr`](crate::RepoErr)，
/// op 是"表名 at 文件:行号"，跨行时是 `db!` 开头那一行。原始的 `DbErr` 挂 source
///
/// ```ignore
/// let user = db!(user::Entity::find_by_id(id).one(txn))?;  // QueryFailed，op "public.user at src/repo.rs:12"
/// db!(am.insert(txn))?;                                    // ActiveModel 的方法也行，InsertFailed
/// ```
///
/// 第二个参数是自定义的 op，拿得到表名时不用写；拿不到表名的写法（下表最后两行）必须写，不写编译报错。
/// 写了时 op 是"自定义, 表名 at 文件:行号"，拿不到表名的是"自定义 at 文件:行号"：
///
/// ```ignore
/// let rows = db!(cake::Entity::find().join(..).into_model::<CakeWithFruit>().all(txn), "cake with fruit")?;
/// ```
///
/// 认的写法：
///
/// | 最后一步 | 错误 | op 里的表名 |
/// |---|---|---|
/// | `Select` 及关联查询的 `one` / `all` / `count` / `stream` 等 | `QueryFailed` | 主实体 |
/// | `Insert` / `InsertMany` / `TryInsert` 的 `exec*` | `InsertFailed` | 实体 |
/// | `UpdateOne` / `UpdateMany` 的 `exec*` | `UpdateFailed` | 实体 |
/// | `DeleteOne` / `DeleteMany` 的 `exec*` | `DeleteFailed` | 实体 |
/// | ActiveModel 的 `insert` / `update` / `update_without_returning` / `delete`，Model 的 `delete` | 对应的增改删 | 实体 |
/// | ActiveModel 的 `save`（主键 NotSet 插入，否则更新） | `ExecFailed` | 实体 |
/// | `into_tuple` / `into_model` / `into_partial_model` / `into_json` 之后、`find_by_statement` | `QueryFailed` | 没有，必须写 op |
/// | `paginate` 之后（分页器） | `QueryFailed` | 没有，必须写 op |
///
/// 其他写法（原生 SQL、`begin`、`load_many`、`cursor_by` 这种要先拿 `&mut` 再执行的）编译不过，用 `.db()`。
/// `stream` 只转开流这一步的错误，流里每一行的 `DbErr` 用 `.db()` 转
#[macro_export]
macro_rules! db {
    // 先找最外层的逗号：前面是调用，后面是自定义 op
    (@args [$($call:tt)+] , $op:expr) => {
        $crate::db!(@split ($op) [] $($call)+)
    };
    (@args [$($call:tt)+]) => {
        $crate::db!(@split () [] $($call)+)
    };
    (@args [$($call:tt)*] $head:tt $($rest:tt)*) => {
        $crate::db!(@args [$($call)* $head] $($rest)*)
    };
    // ActiveModel 自己的方法：操作看方法名，表名从 A::Entity 取。delete 也可能是 Model 的（ModelTrait::delete）
    (@split ($($op:expr)?) [$($recv:tt)+] . insert ($($args:tt)*)) => {
        $crate::db!(@active_model ($($op)?) [$($recv)+] insert ($($args)*) Insert)
    };
    (@split ($($op:expr)?) [$($recv:tt)+] . update ($($args:tt)*)) => {
        $crate::db!(@active_model ($($op)?) [$($recv)+] update ($($args)*) Update)
    };
    (@split ($($op:expr)?) [$($recv:tt)+] . update_without_returning ($($args:tt)*)) => {
        $crate::db!(@active_model ($($op)?) [$($recv)+] update_without_returning ($($args)*) Update)
    };
    (@split ($($op:expr)?) [$($recv:tt)+] . delete ($($args:tt)*)) => {
        $crate::db!(@active_model ($($op)?) [$($recv)+] delete ($($args)*) Delete)
    };
    (@split ($($op:expr)?) [$($recv:tt)+] . save ($($args:tt)*)) => {
        $crate::db!(@active_model ($($op)?) [$($recv)+] save ($($args)*) Exec)
    };
    // 其他：最后一步之前是语句，操作和表名看语句类型
    (@split ($($op:expr)?) [$($recv:tt)+] . $method:ident ($($args:tt)*)) => {{
        #[allow(unused_mut)]
        let mut stmt = $($recv)+;
        let err = $crate::db!(@stmt_err ($($op)?) stmt);
        $crate::seaorm::__private::wrap(stmt.$method($($args)*).await, err)
    }};
    (@split ($($op:expr)?) [$($recv:tt)*]) => {
        compile_error!("db! 的最后一步要是执行方法，比如 db!(user::Entity::find().all(db))")
    };
    // 一个一个往 recv 里挪，直到只剩最后的 `.方法(参数)`
    (@split ($($op:expr)?) [$($recv:tt)*] $head:tt $($rest:tt)*) => {
        $crate::db!(@split ($($op)?) [$($recv)* $head] $($rest)*)
    };
    // 没写 op 的要求语句拿得到表名（TableStmt），写了的什么语句都行
    (@stmt_err () $stmt:ident) => {
        $crate::seaorm::__private::stmt_err(&$stmt, concat!(file!(), ":", line!()))
    };
    (@stmt_err ($op:expr) $stmt:ident) => {
        $crate::seaorm::__private::stmt_err_with(&$stmt, $op, concat!(file!(), ":", line!()))
    };
    (@active_model ($($op:expr)?) [$($recv:tt)+] $method:ident ($($args:tt)*) $kind:ident) => {{
        let am = $($recv)+;
        let err = $crate::seaorm::__private::active_model_err(
            &am,
            $crate::seaorm::__private::Kind::$kind,
            $crate::db!(@opt ($($op)?)),
            concat!(file!(), ":", line!()),
        );
        $crate::seaorm::__private::wrap(am.$method($($args)*).await, err)
    }};
    (@opt ()) => { ::core::option::Option::None::<&'static str> };
    (@opt ($op:expr)) => { ::core::option::Option::Some($op) };
    ($($call:tt)+) => {
        $crate::db!(@args [] $($call)+)
    };
}

/// `db!` 展开后用的，不是公开 API
#[doc(hidden)]
pub mod __private {
    use super::*;

    /// 失败时用哪个错误
    #[derive(Debug, Clone, Copy)]
    pub enum Kind {
        Query,
        Insert,
        Update,
        Delete,
        Exec,
    }

    impl Kind {
        fn err(self, op: String) -> HyErr {
            match self {
                Kind::Query => err!(RepoErr::QueryFailed, op),
                Kind::Insert => err!(RepoErr::InsertFailed, op),
                Kind::Update => err!(RepoErr::UpdateFailed, op),
                Kind::Delete => err!(RepoErr::DeleteFailed, op),
                Kind::Exec => err!(RepoErr::ExecFailed, op),
            }
        }
    }

    /// SeaORM 的语句类型：失败时用哪个错误、op 里写哪张表
    pub trait Stmt {
        /// 失败时用哪个错误
        const KIND: Kind;

        /// 语句的表名，SeaORM 的类型里没有实体时是 None
        fn table() -> Option<String>;
    }

    /// 拿得到表名的语句（[`Stmt::table`] 是 Some），`db!` 不写 op 时要求这个
    #[diagnostic::on_unimplemented(
        message = "db! 拿不到 `{Self}` 的表名，写成 db!(.., \"op\")",
        label = "into_model / into_tuple / into_json / find_by_statement / paginate 之后没有实体，要自己写 op"
    )]
    pub trait TableStmt: Stmt {}

    /// 不写 op 时语句的错误，`at` 是调用位置。只借用来推类型，`use<S>` 让返回值不带着这个借用
    pub fn stmt_err<S: TableStmt>(_: &S, at: &'static str) -> impl FnOnce() -> HyErr + use<S> {
        move || S::KIND.err(op(None::<&str>, S::table(), at))
    }

    /// 写了 op 时语句的错误
    pub fn stmt_err_with<S: Stmt, O: Display>(
        _: &S,
        custom: O,
        at: &'static str,
    ) -> impl FnOnce() -> HyErr + use<S, O> {
        move || S::KIND.err(op(Some(custom), S::table(), at))
    }

    /// ActiveModel 方法的错误，操作由宏按方法名给。收 `IntoActiveModel`：ActiveModel 自己（SeaORM 的 blanket impl）
    /// 和 Model 都满足，`model.delete(db)`（`ModelTrait::delete`）和 `am.delete(db)` 同名，宏分不出来
    pub fn active_model_err<M, A, O>(
        _: &M,
        kind: Kind,
        custom: Option<O>,
        at: &'static str,
    ) -> impl FnOnce() -> HyErr + use<M, A, O>
    where
        M: IntoActiveModel<A>,
        A: ActiveModelTrait,
        O: Display,
    {
        move || kind.err(op(custom, Some(table::<A::Entity>()), at))
    }

    pub fn wrap<T>(
        result: std::result::Result<T, DbErr>,
        err: impl FnOnce() -> HyErr,
    ) -> Result<T> {
        result.wrap_err(err)
    }

    /// "自定义, 表名 at 位置"，没有的部分不写
    fn op(custom: Option<impl Display>, table: Option<String>, at: &str) -> String {
        match (custom, table) {
            (Some(custom), Some(table)) => format!("{custom}, {table} at {at}"),
            (Some(custom), None) => format!("{custom} at {at}"),
            (None, Some(table)) => format!("{table} at {at}"),
            (None, None) => at.to_owned(),
        }
    }

    /// 给 SeaORM 的语句类型实现 [`Stmt`] 和 [`TableStmt`]：`实体` 是 op 里取表名的实体
    macro_rules! impl_stmt {
        ($(<$($g:ident $(: $b:path)?),*> $ty:ty => $entity:ty, $kind:ident;)*) => {$(
            impl<$($g $(: $b)?),*> Stmt for $ty {
                const KIND: Kind = Kind::$kind;

                fn table() -> Option<String> {
                    Some(table::<$entity>())
                }
            }

            impl<$($g $(: $b)?),*> TableStmt for $ty {}
        )*};
    }

    // 关联查询（`find_also_related`、`find_with_related` 等）取主实体 E 的表名
    impl_stmt! {
        <E: EntityTrait> Select<E> => E, Query;
        <E: EntityTrait, F: EntityTrait> SelectTwo<E, F> => E, Query;
        <E: EntityTrait, F: EntityTrait> SelectTwoMany<E, F> => E, Query;
        <E: EntityTrait, F: EntityTrait> SelectTwoRequired<E, F> => E, Query;
        <E: EntityTrait, F: EntityTrait, G: EntityTrait, TOP: Topology> SelectThree<E, F, G, TOP> => E, Query;
        <E: EntityTrait, F: EntityTrait, G: EntityTrait, TOP: Topology> SelectThreeMany<E, F, G, TOP> => E, Query;
        <E: EntityTrait, F: EntityTrait, G: EntityTrait, H: EntityTrait, TOP: Topology> SelectFour<E, F, G, H, TOP> => E, Query;
        <E: EntityTrait, F: EntityTrait, G: EntityTrait, H: EntityTrait, TOP: Topology> SelectFourMany<E, F, G, H, TOP> => E, Query;
        <E: EntityTrait, F: EntityTrait, G: EntityTrait, H: EntityTrait, I: EntityTrait, TOP: Topology> SelectFive<E, F, G, H, I, TOP> => E, Query;
        <E: EntityTrait, F: EntityTrait, G: EntityTrait, H: EntityTrait, I: EntityTrait, J: EntityTrait, TOP: Topology> SelectSix<E, F, G, H, I, J, TOP> => E, Query;
        <A: ActiveModelTrait> Insert<A> => A::Entity, Insert;
        <A: ActiveModelTrait> InsertMany<A> => A::Entity, Insert;
        <A: ActiveModelTrait> TryInsert<A> => A::Entity, Insert;
        <A: ActiveModelTrait> UpdateOne<A> => A::Entity, Update;
        <A: ActiveModelTrait> ValidatedUpdateOne<A> => A::Entity, Update;
        <E: EntityTrait> UpdateMany<E> => E, Update;
        <E: EntityTrait> DeleteOne<E> => E, Delete;
        <E: EntityTrait> ValidatedDeleteOne<E> => E, Delete;
        <E: EntityTrait> DeleteMany<E> => E, Delete;
    }

    // `into_tuple` / `into_model` / `into_json`、`find_by_statement`、`paginate` 之后的类型里已经没有实体，
    // 只实现 Stmt，db! 要写 op
    impl<S: SelectorTrait> Stmt for Selector<S> {
        const KIND: Kind = Kind::Query;

        fn table() -> Option<String> {
            None
        }
    }

    impl<S: SelectorTrait> Stmt for SelectorRaw<S> {
        const KIND: Kind = Kind::Query;

        fn table() -> Option<String> {
            None
        }
    }

    impl<'db, C: ConnectionTrait, S: SelectorTrait + 'db> Stmt for Paginator<'db, C, S> {
        const KIND: Kind = Kind::Query;

        fn table() -> Option<String> {
            None
        }
    }
}
