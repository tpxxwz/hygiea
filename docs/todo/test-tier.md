# 测试分层宏待办

## IDE 里不显示测试的运行按钮

`#[hygiea_test::container]` / `#[hygiea_test::live]` 标在模块上时，RustRover 不在测试函数旁边显示运行按钮；
同一个文件里手写 `#[ignore]` 的普通测试模块能正常显示。

### 调查结论（2026-10-09）

原来以为是宏把模块内容挪进层名子模块、源码位置对不上导致的，**实测不是**。真正的触发条件是三样同时出现：

1. 模块上挂了属性宏（我们的 `#[container]` 和第三方的 `#[serial_test::serial]` 都一样）
2. 模块里有非测试的 `async fn`（辅助函数）
3. 模块里有 `#[tokio::test]` 测试

组件的容器测试（`pg.rs`、`redis.rs`、`s3.rs`）都有 `start()`、`with_resources()` 这类 async 辅助函数，正好命中。

实测结果（`playground/tests/ide_run_marker.rs`、`hygiea-redis/tests/redis.rs` 末尾的探针）：

| 写法 | 按钮 |
|---|---|
| 模块不挂宏，`#[test]` + `#[ignore]` | 有 |
| 模块挂第三方宏，里面只有 `#[test]` | 有 |
| 函数上挂第三方宏 + `#[test]` | 有 |
| 模块挂我们的宏（挪模块的旧版 / 不挪模块的新版都试过），里面只有 `#[test]` | 有 |
| 模块挂我们的宏，里面只有 `#[tokio::test]`（测试函数体为空） | 有 |
| 模块挂我们的宏，async 辅助函数 + 同步 `#[test]` | 有 |
| 模块不挂宏，async 辅助函数 + `#[tokio::test]` | 有 |
| 模块挂我们的宏，async 辅助函数 + `#[tokio::test]`（两份一模一样的，结果一致，排除 IDE 不稳定） | **没有** |
| 模块挂第三方 `#[serial_test::serial]`，async 辅助函数 + `#[tokio::test]` | **没有** |
| 模块不挂宏，`#[tokio::test]` + 函数上挂 `#[serial_test::serial]`，有 async 辅助函数 | 有 |

排除过的猜测：

- 挪模块：不挪模块的版本在简单情况下有按钮，挪模块的旧版在简单情况下也有，不是原因
- `hygiea_test::container` 同时是模块和宏的名字冲突：开了 `container` feature 照样有按钮
- `use hygiea_test::container;` 后写短名 `#[container]`：改成完整路径后 `mod redis` 仍然没有
- 文件头的 `#![cfg(feature = ..)]`：同文件里不挂宏的探针有按钮
- 宏输出补 `#[cfg(test)]`：不影响

原因推测（没在 RustRover 源码里核实）：模块挂了属性宏，整个模块都成了宏的输入，RustRover 要在展开结果里再展开
`#[tokio::test]`（宏套宏）；这时有 async 辅助函数就认不出测试。为什么偏偏是 async 辅助函数没有解释。

另外：`hygiea-test/src/container.rs` 里的单元测试（`src/` 下，不是 `tests/`）在简单情况下也不显示，没查清原因，暂时不管。

### 下一步的方向

宏改成标在测试函数上，模块上不挂宏（对应上表最后一行；换成我们自己的宏后要再实测一次）：

```rust
mod redis_container {
    async fn start() -> RunningContainer { .. }

    #[tokio::test]
    #[hygiea_test::container]   // 只加 #[ignore]；live 再插环境变量检查
    async fn set_and_get_value() { .. }
}
```

- 函数级的宏看不到外层模块名，没法检查模块名带层名后缀；按层筛选（`-- --ignored _container::`）只能靠模块名约定，
  写错的后果是 CI 漏跑，不是误跑 live
- live 只靠 `#[ignore]` + `require_env` 不够：`cargo test -- --ignored`（不带筛选串）、`--include-ignored`、
  筛选串写错（比如 `put_get` 同时匹配 container 和 live）都会选中 live，本机正好导出了凭证就会真连。
  打算加运行时开关：live 测试开头检查 `HYGIEA_TEST_LIVE=1`，没设就 panic
- 实在不行就模块、函数都不挂宏，手写 `#[ignore]`，live 只靠运行时开关

### 相关分支

- `wip/test-tier-suffix`：宏不挪模块，模块名必须以 `_container` / `_live` 结尾（编译期检查），宏固定加 `#[cfg(test)]`，
  组件测试模块已改名、文档已改。在 `tests/` 简单情况下有按钮，但同样解决不了上面 async 的问题
- `wip/test-tier-nested-cfg`：旧版（挪模块）加 `#[cfg(test)]`，带着本次调查的探针：
  `playground/tests/ide_run_marker.rs`（A～H）、`hygiea-redis/tests/redis.rs` 末尾的 `probe_*`，
  `pg.rs` / `redis.rs` 的宏改成了完整路径。都是临时的，定了方向后清理

### 跟着改的

- `hygiea-test-macros` 的实现和 trybuild 快照
- `hygiea-test/src/lib.rs` 里测试分层的文档、AGENTS.md 的测试分层表和运行命令
- 用到宏的测试文件：hygiea-db `tests/pg.rs`、hygiea-redis `tests/redis.rs`、hygiea-aws `tests/s3.rs`、
  hygiea-test `src/container.rs`
