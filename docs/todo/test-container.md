# container 层测试待办

取舍见 `docs/design/test-container.md`。

## 限制同时起的容器数（没定）

nextest 每个测试一个进程，默认按 CPU 核数并行；container 层的测试每个都自己起容器，同时起的容器数就是并行数。
本机（10 核，Docker 分了 8 核、32G）跑 hygiea-db 的 77 个 container 层测试没出过问题，31 秒跑完、没有留下容器。
弱一点的机器或 CI 上可能碰到：

- 同时起太多，内存、CPU 不够，MySQL 这类启动慢的超过启动时限，测试 panic
- 本机没有镜像时第一次拉，碰到 Docker Hub 匿名拉取的频率限制（拉过一次之后用缓存）

限制要放在 nextest 的配置里：每个测试是单独的进程，hygiea-test 里的锁、信号量管不到别的进程。

```toml
# .config/nextest.toml
[test-groups]
container = { max-threads = 4 }

[[profile.default.overrides]]
filter = "test(/::container::/)"
test-group = "container"
```

普通测试照样按核数并行，只有 container 层最多同时跑 `max-threads` 个，代价是 container 层整体慢一些。
使用方（比如 dict-jp）要限的话在它自己的 nextest 配置里加同样一段。

- [ ] 要不要加、上限设几（等真碰到了再定）
- [ ] 加的话，hygiea-test 的 crate 文档里给使用方写一下这段配置
