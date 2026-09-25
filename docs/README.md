# 文档

项目的设计记录、审查结果和调研笔记都放在这里。根目录的 `README.md` 是项目介绍，留在原处（GitHub 和 crates.io 默认读取根目录的 README）。

## reviews/ 审查记录

| 文档 | 日期 | 内容 |
|---|---|---|
| [2026-09-25-core-review.md](reviews/2026-09-25-core-review.md) | 2026-09-25 | hygiea-core 除 http、redact、ws、rate_limit 之外各模块的审查，问题带编号，逐项勾选处理 |
| [date-design-review.md](reviews/date-design-review.md) | 较早 | date 模块的设计审查（当时文件还叫 `src/date.rs`，现在是 `src/datetime/`） |

## notes/ 调研笔记

| 文档 | 内容 |
|---|---|
| [virtual-time-and-parallelism.md](notes/virtual-time-and-parallelism.md) | tokio 虚拟时间 API、多线程下的限制、全局虚拟时钟的几种实现方案，配合 `hygiea-core/examples/backtest_parallel.rs` 阅读 |

## 约定

- 新增文档按用途放进对应子目录，没有合适的就新建一个，并在这里登记一行。
- 文件名用小写加连字符；带日期的审查记录用 `YYYY-MM-DD-主题.md`。
