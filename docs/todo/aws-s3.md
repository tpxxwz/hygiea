# S3 待办

现在的行为见 `hygiea-components/hygiea-aws/src/s3/mod.rs` 和各文件的文档。下面是还没做、还没定的。

## 分片上传（没做，用 transfer manager 还是自己实现没定）

`S3BucketHandle::put` 是一次 PutObject，单个对象最大 5GB。要做的：

- 按大小自动选：不超过阈值一次 PutObject，超过走 CreateMultipartUpload → 并发 UploadPart → CompleteMultipartUpload，
  中途失败 AbortMultipartUpload 后返回原来的错误
- 整个对象的 `x-amz-checksum-sha256` 只用在一次 PutObject 上：分片上传时 sha256 是每片各带各的，内容被改的校验要改成按片做
- 片大小固定（R2 要求除最后一片外大小相同）；按配置的片大小超过 10000 片时自动放大片大小，不报错
- 文件按偏移读出每一片，不整个读进内存；`Bytes` 直接切片
- 配置放在客户端下（`[s3.default.transfer]`、`[s3.extra.<名字>.transfer]`）：阈值、片大小、并发数，启动时检查，不合法报 `AppErr::InvalidConfig`
- container 层用 RustFS 测：超过阈值的文件和 `Bytes` 走分片、读回来核对内容

两种做法：

- 用 aws-sdk-s3-transfer-manager（见下面的调查）：内部的客户端 `AwsS3Client` 里多持有一个它的客户端，上传交给它
- 自己实现：用 `AwsS3Client` 里现有的 SDK 客户端调上面那几个分片接口，不加依赖

## 调查：aws-sdk-s3-transfer-manager

AWS 官方的分片上传、下载库：

- crate 名 `aws-sdk-s3-transfer-manager`（GitHub 仓库是 awslabs/aws-s3-transfer-manager-rs），调查时 crates.io 上是 0.3.1，
  README 写着 Developer Preview、不建议生产用
- 依赖 aws-config、aws-sdk-s3 时开着默认 feature，打开就带回旧版 HTTPS 客户端（hyper 0.14 + rustls 0.21）、SSO 的两个 SDK crate、
  sigv4a，另外还有 hickory-dns、crossbeam、hdrhistogram 等
- 配置只收 S3 的配置（`Config::builder().s3_config(..)`），不能复用已有的 `aws_sdk_s3::Client`：自己再建一个 S3 客户端，
  默认还注入自己的 HTTP 客户端（`enable_runtime_http`），可能有自己的工作线程（`runtime_mode`）
- 分片大小、阈值（`PartSize`，单位字节）和并发（`ConcurrencyMode`）默认都是自动
- 没有承诺支持 R2、RustFS：R2"除最后一片外大小相同"、按分片号读、新的校验和请求头都没验证过

## 自定义域名 + CDN 的公开地址（没做，等要用 R2 的自定义域名时再定）

现在给外部的只有预签名 URL（可以用 `presign_public_origin` 换成自己的域名，经反向代理原样转给服务），
不走 CDN 缓存。想走 Cloudflare 的 CDN，只能给 R2 的 bucket 绑自定义域名：

- R2 的预签名 URL 只能用 S3 API 的域名（`<ACCOUNT_ID>.r2.cloudflarestorage.com`），不能用在自定义域名上；这个域名不走 CDN 缓存
- 自定义域名是公开访问：知道 URL 就能读，不过期，走 CDN 缓存
- 自定义域名上要鉴权：WAF 的 HMAC 校验（Pro 及以上套餐），或者 Worker、Snippets 自己验 token，都不是 S3 预签名

RustFS、MinIO 设了匿名读的策略以后也能公开访问，URL 和预签名的一样（经代理的地址 + `/<bucket>/<key>`），只是不带签名的 query。

要做的话，倾向这样配（客户端级写一次地址，bucket 上只开关，加 bucket 时不用再写地址）：

```toml
[s3.default]                                     # RustFS
public_url = "https://wjjzst.com/cdn/{bucket}"   # bucket 名在路径里，经反向代理

[s3.extra.r2]
public_url = "https://{bucket}.wjjzst.com"       # bucket 名在子域名里，Cloudflare 上按这个规则给每个 bucket 绑域名

[s3.extra.r2.buckets.storage]
name = "dict-jp"
public = true                                    # → https://dict-jp.wjjzst.com/<key>
```

- `{bucket}` 换成实际的 bucket 名（`name`），后面接 `/` 和完整 key（带前缀的句柄加上它的前缀），不签名、不过期
- R2 不能用「地址 + `/bucket名`」：自定义域名绑在一个 bucket 上，路径里再带 bucket 名会被当成 key 的一部分，所以 bucket 名放在模板里由使用方决定放哪
- 启动时检查：bucket 写了 `public = true`、客户端没配 `public_url` 报 `InvalidConfig`；模板里没有 `{bucket}` 也报错（几个 bucket 会拼出同一个地址）
- bucket 公开不公开（R2 绑域名、RustFS 设匿名读的策略）在服务那边设，hygiea 不管；配了却没开，前端拿到的 URL 是 403
- 没定的：方法只加 `public_url(key) -> Option<String>`，还是再加一个统一的 `url(key)`（公开的给公开地址，不公开的按有效期上限签名）；
  用 HMAC 鉴权的再看要不要按 Cloudflare 的格式生成带 token 的 URL

## 删 bucket 的方法放在哪、要不要留（没定，先留着）

`S3Bucket::delete_bucket`、`empty_and_delete_bucket`（`s3-bucket-admin`）挂在不带前缀的句柄 `S3Bucket` 上（`S3BucketWithPrefix` 上没有）：
只删得到配置里的 bucket，也不和 SDK 的 `Client::delete_bucket` 撞名。
现在主要是测试在用（清理临时 bucket、造「bucket 不存在」的场景）。没定的：

- 删完之后，这个 bucket 的句柄（`AwsS3Store` 里的、别处 clone 走的）都还在，再用报 `NoSuchBucket`；配置里没去掉的话，
  重启时开了 `manage_buckets` 会重新建、没开启动失败。从 `AwsS3Store` 的 map 里删掉也解决不了已经发出去的句柄，还要给 store 加锁，
  所以没做；bucket 被控制台、别的服务删掉时句柄同样不知道，按每次操作的报错处理
- 可选的方向：挪到 `AwsS3Store` 上按客户端名换个名字（能删这个客户端上配置以外的 bucket）；或者去掉这两个方法，测试里直接调 SDK 清理，
  `s3-bucket-admin` 只管启动时建 bucket，同时去掉只为它们服务的 `BucketAdminDisabled`、`DeleteObjectsFailed`、`InvalidResponse`
