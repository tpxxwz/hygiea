# 测试分层宏待办

## IDE 里不显示测试的运行按钮

`#[hygiea_test::container]` / `#[hygiea_test::live]` 标在模块上时，RustRover 不在测试函数旁边显示运行按钮；
同一个文件里手写 `#[ignore]` 的普通测试模块能正常显示。

原因：宏做了两件事——

1. 给模块里的测试加 `#[ignore]`
2. 把模块内容挪进以层名命名的子模块（`pg_tests` → `pg_tests::container`），让测试路径带上层名，
   好用 `cargo test -- --ignored ::container::` 只跑某一层

第 2 步让源码里函数的位置和编译后的位置对不上，IDE 认不出测试函数。

### 做法

宏只做第 1 步，函数原地不动。代价是测试路径里没有层名，`::container::` / `::live::` 的筛选用不了
（cargo 不能按 ignore 的原因筛选，`--ignored` 会把两层一起跑）。按层筛选的补救二选一：

- 模块名约定带层名（`pg_container`、`r2_live`），宏检查模块名，不符合约定编译报错；筛选写 `-- --ignored container`
- 不再支持按层筛选，按模块路径跑（`cargo test -p .. pg_tests -- --ignored`）

改完要实际在 RustRover 里确认按钮出来了（属性宏展开后 IDE 能否对应回源码，只能实测）。

### 跟着改的

- `hygiea-test-macros` 的实现和 trybuild 快照
- `hygiea-test/src/lib.rs` 里测试分层的文档、AGENTS.md 的测试分层表和运行命令
- 用到宏的测试文件头写的运行命令：hygiea-db `tests/pg.rs`、hygiea-redis、hygiea-aws 等
