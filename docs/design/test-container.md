# hygiea-test 的容器：为什么这样做

怎么用、默认值、每个方法的行为在模块文档里：`hygiea-test/src/container/mod.rs`（`ContainerSpec`、`RunningContainer`）和
`hygiea-test/src/container/service.rs`（`PostgresSpec`、`MySqlSpec`、`RedisSpec`、`S3Spec`），docs.rs 上是 `hygiea_test::container`。
这里只讲取舍。

---

## 1. 常用服务做成预设，组件自己的测试也用它

hygiea-test 是给使用方（比如 dict-jp）写测试用的。PostgreSQL、MySQL、Redis、S3 这几种，每个使用方的测试都要起，
写法都一样（镜像、账号环境变量、端口、怎样算就绪），所以做成预设：默认值直接能用，每一项都能改。

hygiea 自己组件的 container 层测试（hygiea-db、hygiea-redis、hygiea-aws）本来就把 hygiea-test 当 dev-dependency，
也直接用这些预设，不另写一套。组件特有的部分（比如把 `RunningPostgres` 拼成 `PgConnectConfig`）留在组件自己的
`tests/common` 里。

没放进 `test-support`，也没另建一个公共 crate：

- `test-support` 不能依赖 hygiea-core（core 把它当 dev-dependency，反过来依赖会在 core 的测试构建里出现两份 core），
  hygiea-test 依赖 core，所以 `test-support` 也不能依赖 hygiea-test
- 另建一个不依赖 core 的 crate 放容器：已发布的 hygiea-test 要依赖它，它就得一起发布，多维护一个 crate，
  换来的只是 core 自己的测试能起容器，而 core 用不到

## 2. 返回连接要用的值，不拼连接配置

预设的返回值（`RunningPostgres` 等）只给地址、端口、账号、库、`url()`，不返回 `PgConnectConfig`、`AwsConfig` 这些：
hygiea-test 只依赖 core，不能依赖组件。连接配置由调用方自己拼。

## 3. 不读环境变量

考虑过按「调用方显式设置 > 环境变量（`BuiltinKey` 的 `LOCAL_PG_USER` 这类）> 默认值」三层取账号。没这样做：
要不要用环境变量、用哪个，调用方自己读了再传给 setter 就行，预设里只有默认值和 setter，行为一眼能看清。
`LOCAL_*_HOST` / `LOCAL_*_PORT` 本来也用不上：容器的地址是映射出来的，只能从运行中的容器取。

## 4. 默认 tag 写完整的版本号

默认值取写的时候最新的稳定版、写完整版本号（对过 Docker Hub 上和 `latest` 的 digest）：PostgreSQL `18.6`、Redis `8.10.2`、
RustFS `1.0.3`。MySQL 的 `latest` 指向 innovation 版（当时是 `26.7.0`），默认用 LTS（`9.7.2`）：innovation 版更新快、改动大。

不默认用 `latest`：

- `latest` 指向的版本会变，镜像出了新的大版本，代码不改测试也可能突然失败
- 本机已经有镜像时直接用、不重新拉，不同机器上的 `latest` 是不同的版本

要最新的调用方自己写 `.tag("latest")`。升级默认版本是手动改代码、跑一遍各组件的 container 层再定。
换到这些默认值时，hygiea-db 的 MySQL 测试从 `8.4` 换到了 `9.7.2`，`8.4` 不再有测试覆盖。

## 5. 没指定的密码、key 随机生成

不在代码里写固定的测试密码：每次启动随机生成，从返回值上读。用标准库的 `RandomState`（按进程随机的哈希种子）加计数生成，
不为这个加依赖；只用在本地起的一次性容器上，不需要密码学强度。

## 6. `customize` 在预设之后执行

`customize(|spec| ..)` 拿到的是已经设好镜像、账号、端口、就绪条件的 `ContainerSpec`，在这个基础上追加或覆盖，
预设没覆盖到的需求（加环境变量、改启动参数、调超时）不用为它加 setter。
