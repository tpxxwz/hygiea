//! 常用服务的容器：PostgreSQL、MySQL、Redis、S3（RustFS）。默认值直接能用，每一项都能改，
//! 返回值上拿连接要用的值（地址、账号、库），连接配置由调用方自己拼。
//!
//! ```ignore
//! let pg = PostgresSpec::new().start().await;                  // postgres:18.6，用户、库都是 postgres，密码随机
//! let url = pg.url();                                          // postgres://postgres:..@localhost:<端口>/postgres
//!
//! let pg = PostgresSpec::new()
//!     .tag("17.11")
//!     .user("app")
//!     .password("..")
//!     .database("app")
//!     .customize(|spec| spec.cmd(["postgres", "-c", "max_connections=200"]))   // 在预设的基础上改 ContainerSpec
//!     .start()
//!     .await;
//! let (host, port) = (pg.host(), pg.port());
//! ```
//!
//! 默认的 tag 写完整版本，换版本用 `.tag(..)`（要最新的写 `.tag("latest")`）。没指定的密码、S3 的 key
//! 每次启动随机生成，从返回值上读。返回值 drop 时删除容器

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

use super::{ContainerSpec, RunningContainer};

/// [`customize`](PostgresSpec::customize) 传进来的修改，按调用顺序在预设的设置之后执行
type Customize = Box<dyn FnOnce(ContainerSpec) -> ContainerSpec + Send>;

/// 依次执行 `customize` 收集的修改
fn apply(mut spec: ContainerSpec, customizes: Vec<Customize>) -> ContainerSpec {
    for customize in customizes {
        spec = customize(spec);
    }
    spec
}

/// 随机的 32 位十六进制串：没指定密码、key 时用。标准库按进程随机的哈希种子，加上计数，不用另加依赖
fn random_secret() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut out = String::with_capacity(32);
    for _ in 0..2 {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        out.push_str(&format!("{:016x}", hasher.finish()));
    }
    out
}

// ---- PostgreSQL ---------------------------------------------------------------

const PG_PORT: u16 = 5432;

/// PostgreSQL 容器：默认 `postgres:18.6`，用户、库都是 `postgres`，密码随机。
/// 镜像启动时先起一个临时实例初始化再重启，就绪日志第二次出现才算就绪
pub struct PostgresSpec {
    tag: String,
    user: String,
    password: Option<String>,
    database: String,
    customizes: Vec<Customize>,
}

impl Default for PostgresSpec {
    fn default() -> Self {
        Self::new()
    }
}

impl PostgresSpec {
    pub fn new() -> Self {
        Self {
            tag: "18.6".to_string(),
            user: "postgres".to_string(),
            password: None,
            database: "postgres".to_string(),
            customizes: Vec::new(),
        }
    }

    /// 镜像 tag，默认 `18.6`
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.tag = tag.into();
        self
    }

    /// 超级用户名（`POSTGRES_USER`），默认 `postgres`
    pub fn user(mut self, user: impl Into<String>) -> Self {
        self.user = user.into();
        self
    }

    /// 密码（`POSTGRES_PASSWORD`），不设每次随机
    pub fn password(mut self, password: impl Into<String>) -> Self {
        self.password = Some(password.into());
        self
    }

    /// 启动时建的库（`POSTGRES_DB`），默认 `postgres`
    pub fn database(mut self, database: impl Into<String>) -> Self {
        self.database = database.into();
        self
    }

    /// 在预设的镜像、环境变量、端口、就绪条件之后，再改底下的 [`ContainerSpec`]（加环境变量、改 cmd、超时等），可以调多次
    pub fn customize(
        mut self,
        f: impl FnOnce(ContainerSpec) -> ContainerSpec + Send + 'static,
    ) -> Self {
        self.customizes.push(Box::new(f));
        self
    }

    /// 启动并等就绪，起不来时 panic，同 [`ContainerSpec::start`]
    pub async fn start(self) -> RunningPostgres {
        let password = self.password.unwrap_or_else(random_secret);
        let spec = ContainerSpec::new("postgres", self.tag)
            .env("POSTGRES_USER", &self.user)
            .env("POSTGRES_PASSWORD", &password)
            .env("POSTGRES_DB", &self.database)
            .port(PG_PORT)
            .wait_log_times("database system is ready to accept connections", 2);
        RunningPostgres {
            container: apply(spec, self.customizes).start().await,
            user: self.user,
            password,
            database: self.database,
        }
    }
}

/// 运行中的 PostgreSQL 容器，drop 时删除
pub struct RunningPostgres {
    container: RunningContainer,
    user: String,
    password: String,
    database: String,
}

impl RunningPostgres {
    /// 从宿主机连用的主机名
    pub fn host(&self) -> &str {
        self.container.host()
    }

    /// 映射到宿主机的端口
    pub fn port(&self) -> u16 {
        self.container.port(PG_PORT)
    }

    pub fn user(&self) -> &str {
        &self.user
    }

    pub fn password(&self) -> &str {
        &self.password
    }

    pub fn database(&self) -> &str {
        &self.database
    }

    /// `postgres://用户:密码@主机:端口/库`。用户、密码原样拼进去，不做 URL 转义
    pub fn url(&self) -> String {
        format!(
            "postgres://{}:{}@{}:{}/{}",
            self.user,
            self.password,
            self.host(),
            self.port(),
            self.database
        )
    }

    /// 底下的容器
    pub fn container(&self) -> &RunningContainer {
        &self.container
    }
}

// ---- MySQL --------------------------------------------------------------------

const MYSQL_PORT: u16 = 3306;

/// MySQL 容器：默认 `mysql:9.7.2`（LTS），用户 `root`，库 `test`，密码随机，启动时限 180 秒。
/// 用户不是 `root` 时另建这个用户（`MYSQL_USER`），`root` 的密码和它一样。
/// 镜像启动时先起一个临时实例初始化再重启，就绪日志第二次出现才算就绪
pub struct MySqlSpec {
    tag: String,
    user: String,
    password: Option<String>,
    database: String,
    customizes: Vec<Customize>,
}

impl Default for MySqlSpec {
    fn default() -> Self {
        Self::new()
    }
}

impl MySqlSpec {
    pub fn new() -> Self {
        Self {
            tag: "9.7.2".to_string(),
            user: "root".to_string(),
            password: None,
            database: "test".to_string(),
            customizes: Vec::new(),
        }
    }

    /// 镜像 tag，默认 `9.7.2`
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.tag = tag.into();
        self
    }

    /// 用户名，默认 `root`；不是 `root` 时另建这个用户，对 [`database`](Self::database) 有全部权限
    pub fn user(mut self, user: impl Into<String>) -> Self {
        self.user = user.into();
        self
    }

    /// 密码，不设每次随机。`root` 也用这个密码
    pub fn password(mut self, password: impl Into<String>) -> Self {
        self.password = Some(password.into());
        self
    }

    /// 启动时建的库（`MYSQL_DATABASE`），默认 `test`
    pub fn database(mut self, database: impl Into<String>) -> Self {
        self.database = database.into();
        self
    }

    /// 在预设之后再改底下的 [`ContainerSpec`]，同 [`PostgresSpec::customize`]
    pub fn customize(
        mut self,
        f: impl FnOnce(ContainerSpec) -> ContainerSpec + Send + 'static,
    ) -> Self {
        self.customizes.push(Box::new(f));
        self
    }

    /// 启动并等就绪，起不来时 panic，同 [`ContainerSpec::start`]
    pub async fn start(self) -> RunningMySql {
        let password = self.password.unwrap_or_else(random_secret);
        let mut spec = ContainerSpec::new("mysql", self.tag)
            .env("MYSQL_ROOT_PASSWORD", &password)
            .env("MYSQL_DATABASE", &self.database)
            .port(MYSQL_PORT)
            .wait_log_times("mysqld: ready for connections", 2)
            .startup_timeout(std::time::Duration::from_secs(180));
        if self.user != "root" {
            spec = spec
                .env("MYSQL_USER", &self.user)
                .env("MYSQL_PASSWORD", &password);
        }
        RunningMySql {
            container: apply(spec, self.customizes).start().await,
            user: self.user,
            password,
            database: self.database,
        }
    }
}

/// 运行中的 MySQL 容器，drop 时删除
pub struct RunningMySql {
    container: RunningContainer,
    user: String,
    password: String,
    database: String,
}

impl RunningMySql {
    /// 从宿主机连用的主机名
    pub fn host(&self) -> &str {
        self.container.host()
    }

    /// 映射到宿主机的端口
    pub fn port(&self) -> u16 {
        self.container.port(MYSQL_PORT)
    }

    pub fn user(&self) -> &str {
        &self.user
    }

    pub fn password(&self) -> &str {
        &self.password
    }

    pub fn database(&self) -> &str {
        &self.database
    }

    /// `mysql://用户:密码@主机:端口/库`。用户、密码原样拼进去，不做 URL 转义
    pub fn url(&self) -> String {
        format!(
            "mysql://{}:{}@{}:{}/{}",
            self.user,
            self.password,
            self.host(),
            self.port(),
            self.database
        )
    }

    /// 底下的容器
    pub fn container(&self) -> &RunningContainer {
        &self.container
    }
}

// ---- Redis --------------------------------------------------------------------

const REDIS_PORT: u16 = 6379;

/// Redis 容器：默认 `redis:8.10.2`，不设密码
pub struct RedisSpec {
    tag: String,
    password: Option<String>,
    customizes: Vec<Customize>,
}

impl Default for RedisSpec {
    fn default() -> Self {
        Self::new()
    }
}

impl RedisSpec {
    pub fn new() -> Self {
        Self {
            tag: "8.10.2".to_string(),
            password: None,
            customizes: Vec::new(),
        }
    }

    /// 镜像 tag，默认 `8.10.2`
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.tag = tag.into();
        self
    }

    /// 要密码（`--requirepass`），不设不要密码。用了 [`customize`](Self::customize) 改 cmd 时这个参数要自己带上
    pub fn password(mut self, password: impl Into<String>) -> Self {
        self.password = Some(password.into());
        self
    }

    /// 在预设之后再改底下的 [`ContainerSpec`]，同 [`PostgresSpec::customize`]
    pub fn customize(
        mut self,
        f: impl FnOnce(ContainerSpec) -> ContainerSpec + Send + 'static,
    ) -> Self {
        self.customizes.push(Box::new(f));
        self
    }

    /// 启动并等就绪，起不来时 panic，同 [`ContainerSpec::start`]
    pub async fn start(self) -> RunningRedis {
        let mut spec = ContainerSpec::new("redis", self.tag)
            .port(REDIS_PORT)
            .wait_log("Ready to accept connections");
        if let Some(password) = &self.password {
            spec = spec.cmd(["redis-server", "--requirepass", password.as_str()]);
        }
        RunningRedis {
            container: apply(spec, self.customizes).start().await,
            password: self.password,
        }
    }
}

/// 运行中的 Redis 容器，drop 时删除
pub struct RunningRedis {
    container: RunningContainer,
    password: Option<String>,
}

impl RunningRedis {
    /// 从宿主机连用的主机名
    pub fn host(&self) -> &str {
        self.container.host()
    }

    /// 映射到宿主机的端口
    pub fn port(&self) -> u16 {
        self.container.port(REDIS_PORT)
    }

    /// 设了密码才有
    pub fn password(&self) -> Option<&str> {
        self.password.as_deref()
    }

    /// `redis://主机:端口`，有密码时 `redis://:密码@主机:端口`。密码原样拼进去，不做 URL 转义
    pub fn url(&self) -> String {
        match &self.password {
            Some(password) => format!("redis://:{password}@{}:{}", self.host(), self.port()),
            None => format!("redis://{}:{}", self.host(), self.port()),
        }
    }

    /// 底下的容器
    pub fn container(&self) -> &RunningContainer {
        &self.container
    }
}

// ---- S3（RustFS） -------------------------------------------------------------

const S3_PORT: u16 = 9000;

/// S3 兼容的对象存储容器，用 RustFS：默认 `rustfs/rustfs:1.0.3`，access key、secret key 随机，`/health` 返回 200 算就绪。
/// 不建 bucket，测试自己用 S3 客户端建；客户端用 path-style、region 随便填
pub struct S3Spec {
    tag: String,
    access_key: Option<String>,
    secret_key: Option<String>,
    customizes: Vec<Customize>,
}

impl Default for S3Spec {
    fn default() -> Self {
        Self::new()
    }
}

impl S3Spec {
    pub fn new() -> Self {
        Self {
            tag: "1.0.3".to_string(),
            access_key: None,
            secret_key: None,
            customizes: Vec::new(),
        }
    }

    /// 镜像 tag，默认 `1.0.3`
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.tag = tag.into();
        self
    }

    /// access key（`RUSTFS_ACCESS_KEY`），不设每次随机
    pub fn access_key(mut self, access_key: impl Into<String>) -> Self {
        self.access_key = Some(access_key.into());
        self
    }

    /// secret key（`RUSTFS_SECRET_KEY`），不设每次随机
    pub fn secret_key(mut self, secret_key: impl Into<String>) -> Self {
        self.secret_key = Some(secret_key.into());
        self
    }

    /// 在预设之后再改底下的 [`ContainerSpec`]，同 [`PostgresSpec::customize`]
    pub fn customize(
        mut self,
        f: impl FnOnce(ContainerSpec) -> ContainerSpec + Send + 'static,
    ) -> Self {
        self.customizes.push(Box::new(f));
        self
    }

    /// 启动并等就绪，起不来时 panic，同 [`ContainerSpec::start`]
    pub async fn start(self) -> RunningS3 {
        let access_key = self.access_key.unwrap_or_else(random_secret);
        let secret_key = self.secret_key.unwrap_or_else(random_secret);
        let spec = ContainerSpec::new("rustfs/rustfs", self.tag)
            .env("RUSTFS_ACCESS_KEY", &access_key)
            .env("RUSTFS_SECRET_KEY", &secret_key)
            .port(S3_PORT)
            .wait_http(S3_PORT, "/health", 200);
        RunningS3 {
            container: apply(spec, self.customizes).start().await,
            access_key,
            secret_key,
        }
    }
}

/// 运行中的 S3（RustFS）容器，drop 时删除
pub struct RunningS3 {
    container: RunningContainer,
    access_key: String,
    secret_key: String,
}

impl RunningS3 {
    /// S3 客户端的 endpoint：`http://主机:端口`
    pub fn endpoint(&self) -> String {
        format!("http://{}", self.container.addr(S3_PORT))
    }

    pub fn access_key(&self) -> &str {
        &self.access_key
    }

    pub fn secret_key(&self) -> &str {
        &self.secret_key
    }

    /// 底下的容器
    pub fn container(&self) -> &RunningContainer {
        &self.container
    }

    /// 用管理员身份建一个普通用户，权限是 `policy`（IAM 策略 JSON，比如只允许读写对象、不允许建 bucket），测权限不够时的行为用。
    /// 在容器里用 curl 调 RustFS 的管理接口（`/rustfs/admin/v3` 下的建策略、建用户、把策略绑给用户，SigV4 签名用 curl 的 `--aws-sigv4`），
    /// 策略名是 `<access_key>-policy`。`access_key`、`secret_key` 原样拼进 URL 和 JSON，只用字母、数字和 `-`。任何一步失败都 panic
    pub async fn add_user(&self, access_key: &str, secret_key: &str, policy: &str) {
        let policy_name = format!("{access_key}-policy");
        let admin = format!("http://127.0.0.1:{S3_PORT}/rustfs/admin/v3");
        self.admin_put(
            &format!("{admin}/add-canned-policy?name={policy_name}"),
            Some(policy),
        )
        .await;
        self.admin_put(
            &format!("{admin}/add-user?accessKey={access_key}"),
            Some(&format!(
                r#"{{"secretKey":"{secret_key}","status":"enabled"}}"#
            )),
        )
        .await;
        self.admin_put(
            &format!(
                "{admin}/set-user-or-group-policy?policyName={policy_name}&userOrGroup={access_key}&isGroup=false"
            ),
            None,
        )
        .await;
    }

    /// 在容器里用管理员身份 PUT 管理接口，不是 2xx 就 panic
    async fn admin_put(&self, url: &str, body: Option<&str>) {
        let user = format!("{}:{}", self.access_key, self.secret_key);
        let mut cmd = vec![
            "curl",
            "-sS",
            "--fail-with-body",
            "--aws-sigv4",
            "aws:amz:us-east-1:s3",
            "--user",
            &user,
            "-X",
            "PUT",
            url,
        ];
        if let Some(body) = body {
            cmd.extend(["-d", body]);
        }
        let output = self.container.exec(cmd).await;
        if output.exit_code != 0 {
            panic!(
                "rustfs admin PUT {url}: exit {} stdout {:?} stderr {:?}",
                output.exit_code, output.stdout, output.stderr
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 随机的密码：32 位十六进制，两次不一样
    #[test]
    fn random_secret_differs() {
        let a = random_secret();
        let b = random_secret();
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}

/// 每个预设起一次，真的连上：PG、MySQL 用 sqlx 查一下，Redis 发 AUTH / PING，S3 查 `/health`
#[cfg(test)]
#[crate::container]
mod start {
    use sqlx::Connection;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    /// 默认值：用户、库、随机的密码都能连上
    #[tokio::test]
    async fn postgres_defaults() {
        let pg = PostgresSpec::new().start().await;
        assert_eq!((pg.user(), pg.database()), ("postgres", "postgres"));
        let mut conn = sqlx::PgConnection::connect(&pg.url()).await.unwrap();
        let one: i32 = sqlx::query_scalar("SELECT 1")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(one, 1);
    }

    /// 改了用户、密码、库，customize 改的启动参数生效
    #[tokio::test]
    async fn postgres_customized() {
        let pg = PostgresSpec::new()
            .user("app")
            .password("app-secret")
            .database("app_db")
            .customize(|spec| spec.cmd(["postgres", "-c", "max_connections=42"]))
            .start()
            .await;
        assert_eq!(pg.password(), "app-secret");
        let mut conn = sqlx::PgConnection::connect(&pg.url()).await.unwrap();
        let (user, database): (String, String) =
            sqlx::query_as("SELECT current_user::text, current_database()::text")
                .fetch_one(&mut conn)
                .await
                .unwrap();
        assert_eq!((user.as_str(), database.as_str()), ("app", "app_db"));
        let max_connections: String = sqlx::query_scalar("SHOW max_connections")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(max_connections, "42");
    }

    /// 默认 root；另建的用户也能连上自己的库
    #[tokio::test]
    async fn mysql_root_and_user() {
        let root = MySqlSpec::new().start().await;
        let mut conn = sqlx::MySqlConnection::connect(&root.url()).await.unwrap();
        let database: String = sqlx::query_scalar("SELECT DATABASE()")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(database, "test");

        let app = MySqlSpec::new()
            .user("app")
            .database("app_db")
            .start()
            .await;
        let mut conn = sqlx::MySqlConnection::connect(&app.url()).await.unwrap();
        let user: String = sqlx::query_scalar("SELECT CURRENT_USER()")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert!(user.starts_with("app@"), "{user}");
    }

    /// 发一条 RESP 命令，读回第一行
    async fn redis_command(stream: &mut tokio::net::TcpStream, args: &[&str]) -> String {
        let mut request = format!("*{}\r\n", args.len());
        for arg in args {
            request.push_str(&format!("${}\r\n{arg}\r\n", arg.len()));
        }
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut buf = vec![0; 256];
        let n = stream.read(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf[..n]).trim_end().to_string()
    }

    /// 不设密码直接能 PING；设了密码要先 AUTH
    #[tokio::test]
    async fn redis_with_and_without_password() {
        let open = RedisSpec::new().start().await;
        assert_eq!(open.password(), None);
        let mut stream = tokio::net::TcpStream::connect(open.container().addr(6379))
            .await
            .unwrap();
        assert_eq!(redis_command(&mut stream, &["PING"]).await, "+PONG");

        let locked = RedisSpec::new().password("redis-secret").start().await;
        assert!(locked.url().starts_with("redis://:redis-secret@"));
        let mut stream = tokio::net::TcpStream::connect(locked.container().addr(6379))
            .await
            .unwrap();
        assert!(
            redis_command(&mut stream, &["PING"])
                .await
                .starts_with("-NOAUTH")
        );
        assert_eq!(
            redis_command(&mut stream, &["AUTH", "redis-secret"]).await,
            "+OK"
        );
        assert_eq!(redis_command(&mut stream, &["PING"]).await, "+PONG");
    }

    /// endpoint 上的 /health 返回 200；没设的 key 是随机的
    #[tokio::test]
    async fn s3_health() {
        let s3 = S3Spec::new().access_key("s3-access").start().await;
        assert_eq!(s3.access_key(), "s3-access");
        assert_eq!(s3.secret_key().len(), 32);
        let status = reqwest::get(format!("{}/health", s3.endpoint()))
            .await
            .unwrap()
            .status();
        assert_eq!(status, 200);
    }

    /// add_user 建的用户按策略来：能读写对象，不能建 bucket。在容器里用 curl 以各自的身份发请求
    #[tokio::test]
    async fn s3_add_user_with_policy() {
        let s3 = S3Spec::new()
            .access_key("admin")
            .secret_key("admin-secret")
            .start()
            .await;
        s3.add_user(
            "limited",
            "limited-secret",
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["s3:GetObject","s3:PutObject"],"Resource":["arn:aws:s3:::*"]}]}"#,
        )
        .await;
        let put = |user: &'static str, url: &'static str| {
            let s3 = &s3;
            async move {
                s3.container()
                    .exec([
                        "curl",
                        "-s",
                        "-o",
                        "/dev/null",
                        "-w",
                        "%{http_code}",
                        "--aws-sigv4",
                        "aws:amz:us-east-1:s3",
                        "--user",
                        user,
                        "-X",
                        "PUT",
                        url,
                    ])
                    .await
                    .stdout
            }
        };
        assert_eq!(
            put("admin:admin-secret", "http://127.0.0.1:9000/existing").await,
            "200"
        );
        assert_eq!(
            put(
                "limited:limited-secret",
                "http://127.0.0.1:9000/existing/a.txt"
            )
            .await,
            "200"
        );
        assert_eq!(
            put("limited:limited-secret", "http://127.0.0.1:9000/other").await,
            "403"
        );
    }
}
