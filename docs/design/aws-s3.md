# S3：内部设计

怎么用（默认和其他的客户端、bucket，带前缀的句柄，上传读删，预签名）、限制都在模块文档里：
`hygiea-components/hygiea-aws/src/s3/mod.rs`（docs.rs 上是 `hygiea::aws`），各类型、方法的细节在它们自己的文档，
预签名 URL 怎么配合反向代理见 `S3ClientConfig` 的 `presign_public_origin`。
这里只讲为什么这样做。还没做的在 [docs/todo/aws-s3.md](../todo/aws-s3.md)。

---

## 1. 客户端和 bucket 两层都是「一个默认的 + 按名字的其他」，按要哪些直接取 bucket

配置里是默认的客户端和按名字的其他客户端（`extra`），每个客户端下又是默认的 bucket 和按段名的其他 bucket（`buckets.extra`），
两层的默认项都必须写。`AwsS3Store` 对外只有取 bucket 句柄的方法，按要哪些客户端、哪些 bucket 分：

| | 默认 bucket | 按段名的其他 bucket |
|---|---|---|
| 默认客户端 | `default_client_default_bucket()` | `default_client_buckets(段名们)` |
| 一个其他客户端 | `client_default_bucket(客户端名)` | `client_buckets(客户端名, 段名们)` |
| 几个其他客户端 | `clients_default_buckets(客户端名们)` | `clients_buckets(客户端名们, 段名们)` |

配置里没有的取不到。句柄在启动时按配置建一次（`S3Bucket::new` 只在 store 里用），之后取的都是 clone。

- 默认项单独一项、必须写：大多数应用只连一个 S3 服务、只用一个 bucket，代码里直接 `default_client_default_bucket()`，
  不用给客户端和 bucket 起名、到处写同一个字符串，也不用处理「没配」；配置里没写，加载配置时就报错。
  考虑过只有一个按名字的 map：只用一个的应用也得给它起名、写死在代码里
- 默认的和其他的分开取：默认的只从默认的方法取，按名字取的参数、结果里都没有默认项。考虑过用 `Option<客户端名>` 表示默认的：
  调用的地方 `None` 要查文档才知道意思，两种的报错也不一样，默认的不会「没配」。考虑过批量取的方法用 `bool` 参数表示带不带默认项、
  结果里默认项放在最前面：`true` / `false` 同样要查文档，默认项在结果里还得用 `default` 当 key，和其他项混在一起，用的时候又要分出来
- 其他的名字不能是空的（代码里没法有意义地按空名字取，多半是写错了），也不能叫 `default`（报错时默认项就用这个名字，撞了分不清）。
  这个名字不导出。考虑过导出给要把默认的和其他客户端的名字记在一起的使用方（比如记一个文件存进了哪些客户端）：
  按名字取的方法都不收默认的名字，拿它记下来的也没法按名字取回来，要不要记、怎么记默认的是使用方自己的规则
- 不对外给「先取客户端、再取 bucket」的中间一层（考虑过 `store.default_endpoint().bucket(..)`）：使用方用的都是 bucket，
  客户端只是个中间步骤
- 批量取的方法给「同一个文件要存进几个 S3」这类场景：一次检查完用到的客户端和 bucket，结果的 key 是客户端名、段名，可以直接存起来，
  之后按名字再取。参数和结果用 `IndexSet` / `IndexMap`，顺序就是调用方给的顺序，名字不会重复。批量取的就是逐个调单个取的方法再收集
- 段名是 trait `S3BucketName`（照 `EnvKey`、`S3KeyPrefix` 的做法）：hygiea 给字符串实现，使用方可以给自己列出全部 bucket 的枚举实现，
  代码里就不用到处写段名的字符串。客户端名不做成 trait：其他客户端的名字常常来自动态配置、要原样存起来，本来就是字符串
- 服务上实际的 bucket 名是段里的 `name`，不写就是段名（默认 bucket 的段名是 `default`）。代码里写死段名，
  各环境的 bucket 名不一样（dev 一个、prod 一个）时只改配置的 `name`，代码不用另读一份 bucket 名的配置
- 同一个客户端下实际的 bucket 名不能重复，`name` 也不能和别的段的段名一样，启动时检查：段名是 TOML 表的 key，写重了解析时就报错，
  但一个段的 `name` 可以和另一个段的段名或 `name` 撞上。两个段指着同一个 bucket 一般是写错了；
  `name` 和别的段的段名一样时，就算实际的名字不重复（`a` 的 `name` 是 `b`，`b` 的 `name` 是 `c`），
  按段名 `b` 取到的也不是实际叫 `b` 的 bucket，读配置时容易看错
- 不暴露 SDK 客户端，句柄上只有对象操作，也不暴露客户端和 bucket 名。考虑过留一个口子给 hygiea 没封装的操作（列对象、复制、分片上传等）直接调 SDK：
  实际用的只有测试（测试按配置自己建 SDK 客户端），要用的时候再加。考虑过从句柄上拿客户端和 bucket 名（`bucket.client()`、`bucket.bucket()`）：
  直接调 SDK 时绕开了句柄的前缀，放在句柄上反而让人以为还在句柄的范围里
- 删 bucket 的方法只在 `S3Bucket` 上（只删得到配置里的、不和 SDK 的 `delete_bucket` 撞名），要不要留见 todo

## 2. 前缀在句柄上，不在配置里

从 `AwsS3Store` 取到的 `S3Bucket` 不带前缀，key 原样发出；要限定在一个前缀下，调用方用 `S3Bucket::with_prefix` 得到
`S3BucketWithPrefix`，key 都加上它的前缀。两种句柄的对象操作都在 trait `S3BucketHandle` 里，按现成的 key 写入的 `put_object` 除外。

- 考虑过 bucket 配置里写根前缀（`root_prefix`），句柄自动加上：配置里的根前缀和 `put` 的 `key_prefix` 是两层前缀，
  读代码时分不清前缀指的是哪一段；一个客户端下同一个 bucket 只有一个句柄，根前缀也只能有一个，想在同一个 bucket 里换一层前缀
  只能另配一个连同一个服务的客户端。前缀做成调用方自己套的句柄，要几层、套在哪由调用方决定，配置里只有 bucket 本身
- `S3BucketWithPrefix` 上不能再 `with_prefix`：只有一层，`put` 返回的 key 相对的是哪个前缀不会有歧义。
  它上面也没有删 bucket 的方法：删的是整个 bucket，和「只管前缀下」对不上
- `put_object` 只在 `S3Bucket` 上：它收的是现成的 key（调用方表里存的、别的句柄 `put` 返回的），在带前缀的句柄上
  很容易把前缀再加一遍。`S3Bucket` 上 key 原样发出，传的就是完整 key；`put` 在两种句柄上都能用，内部拼好完整 key
  再调底下 `S3Bucket` 的 `put_object`
- 句柄不暴露拼好的完整 key（考虑过 `full_key`）：直接调 SDK 时前缀是调用方建句柄时自己给的，自己拼上就行；
  hygiea 只多做一件补结尾 `/` 的事，为它开一个方法，实际用的只有测试
- 对象操作写成 trait 的默认实现，两种句柄只提供「底下的 bucket」和「完整 key 怎么拼」（放在私有模块的 `Sealed` 里，
  外面实现不了这个 trait，也调不到这两个方法）。考虑过两种句柄各写一遍、或者带前缀的转发给 `S3Bucket`：方法一多就是两份；
  考虑过 `S3BucketWithPrefix` `Deref` 到 `S3Bucket`：`prefixed.get(key)` 会直接调到 `S3Bucket::get`，绕开前缀
- trait 里的方法是 `async fn`，没有写成 `-> impl Future + Send`：`put` 的 `decide` 是 `AsyncFnOnce`，它返回的 future 没法约束成 `Send`。
  在具体的句柄类型上调用时编译器知道 future 是什么，推得出 `Send`；只有写成泛型（`T: S3BucketHandle`）的代码推不出
- 前缀是 trait `S3KeyPrefix`，照 `EnvKey` 的做法：hygiea 给字符串实现，使用方可以给自己的类型（比如列出全部文件类型的枚举）实现，
  业务代码就只能传定义好的类型。考虑过检查过的结构体（只能经 `new` 建）：`new` 要检查、返回 `Result`，不是 `const fn`，
  使用方没法把固定的前缀写成常量，只能每次用时建或者启动时建好存着；前缀写死在枚举里时，trait 直接返回字符串就行。
  检查、规整（不空、不以 `/` 开头、没有 `//`、补上结尾的 `/`）在 `put`、`with_prefix` 用的时候做，规则只写在一个内部函数里，
  代价是每次 `put` 都检查一遍（只是一个短字符串）。空串不当作「没有前缀」，没有前缀就不套 `with_prefix`

## 3. 上传由 `put` 拼 key，传不传由调用方决定

上传是 `S3BucketHandle::put(key_prefix, key, file, decide)`：读一遍内容，算出 sha256、大小，定下 Content-Type 和扩展名
（`PutDigest`），交给调用方的 `decide` 决定跳过还是上传；上传的 key（相对句柄）由 `put` 拼成 `key_prefix` + 主文件名 + `.` + 扩展名
（没有扩展名的不带 `.`），存进调用方表里的就是它。主文件名按 `PutKey` 定：`Content` 是 sha256，`Named` 是调用方给的名字。

**默认按内容拼 key（`PutKey::Content`）**

- 内容一样 key 就一样：调用方按 sha256 查自己的表就能去重；内容一换 key 就换，key 上的对象不会被改，URL 可以长期缓存，
  前端不会拿到旧内容。要换内容（比如换头像）是存一个新对象，再改调用方表里指向的 key
- 考虑过 `decide` 返回 key：key 是什么都行，sha256 和内容对不对得上只能靠调用方自己（拿 A 的 sha256 拼 key、传的是 B），
  key 合不合规（空的、`/` 开头、`//`）也要另加检查或者另定一个 key 类型。key 在 `put` 里按这次算出的 sha256 拼，这两件事都不用管了

**要独立对象的给名字（`PutKey::Named`）**

- 同样的内容要存成各自独立的对象（标记文件、占位、用户各自上传的文件），按内容拼会合成一个；这时调用方给主文件名，
  名字一样就覆盖。摘要、类型、扩展名、sha256 校验照旧由 `put` 做，调用方不用自己算
- 只给主文件名，前缀和扩展名照样由 `put` 拼：前缀的规则两种一样，扩展名的定法也一样（认得出来的用认出的）。
  名字可以有多层、可以有 `.`，不能是空的、`/` 开头、有 `//`，在读内容之前检查
- 「命名方式」和「内容来源」是两件互不相关的选择，所以 `PutKey` 和 `PutFile` 分成两个 enum，任意组合；
  考虑过合成一个 enum：要么把组合一个个列出来，要么每种来源都带上命名字段，都会重复
- 按内容拼的 key 是 `前缀/<64 位十六进制>[.扩展名]`，自定义的名字几乎不会和它撞上；两种对象最好还是放在不同的前缀下，列目录、清理时不混在一起
- 已经知道 key、sha256、大小、Content-Type 的直接调 `S3Bucket::put_object(key, body, sha256_base64, size, content_type)`，就是一次 PutObject，
  比如把 `put` 传过的对象补传到另一个 S3：这些都记在调用方的表里，原样传进来，sha256、大小照样作为 `x-amz-checksum-sha256`、
  `Content-Length` 让服务端校验。参数顺序和 `PutDigest`、`PutFile::Stream` 一样：位置、内容、sha256、大小、Content-Type。`body` 收 `impl Into<ByteStream>`：内存里的内容直接传，另一个 S3 `get`
  读出来的流也能直接传，边读边传，不进内存

**扩展名尽量由内容定，文件名不用**

- `put` 不收文件名（给用户看的名字），`Named` 给的是 key 里的名字，两回事：上传时不写 Content-Disposition，
  下载时要指定文件名的在签名时给（见第 5 节）
- 扩展名先看文件头：认得出来的用认出的扩展名（给 `jpeg` 认出 `jpg` 的用 `jpg`），同一份内容扩展名总是一样，不受调用方写法影响。
  认不出来的（纯文本、JSON 这类没有固定文件头的）才用 `PutFile` 带的扩展名，没带就没有扩展名
- 带的扩展名要和认出的对得上，对不上报错：只看扩展名的话改个名字就能骗过去，比如把下载失败拿到的 HTML 错误页当 mp3 存
- 带的扩展名的规则（不空、只能是字母数字）在读内容之前检查，不对的不读内容、不调 `decide`；转成小写，`MP3` 和 `mp3` 不会变成两个 key

**摘要在 `put` 里算，交给钩子**

- 去重要查调用方自己的表，hygiea 不知道怎么查，所以留一个钩子 `decide`；它还能核对 sha256（比如补传时和表里记的比）、
  核对扩展名（比如和表里已有的对不上时报错）、报错中止
- Content-Type、扩展名在 `decide` 之前就定好，放进 `PutDigest`：调用方在决定之前就能拿它们和自己的记录比，跳过时也用得上。
  代价是跳过的也要识别一次，只看开头几 KB，可以忽略
- 考虑过调用方先算好摘要再传进 `put`：`put` 没法确认传进来的摘要就是这次的内容。考虑过先上传、再把摘要返回给调用方查：
  已经传上去了，去重就晚了
- `PutDecision::Skip(T)` 带一个值，原样经 `PutOutcome::Skipped(T)` 交回调用方（比如表里已有记录的 id）。考虑过 `decide` 只返回 `bool`：
  这个值就得放进闭包外面的变量里，跳过和值分在两处，编译器不再保证跳过时一定有值，调用方要多写一个不可能发生的分支

**上传的内容能重复读**

`put` 收 `PutFile`：内存里的 `Bytes` 加可选的扩展名、本地文件的路径，或者只能读一遍的流（`ByteStream`）带上摘要：

- `Bytes`、本地文件要读两遍：先算摘要，`decide` 决定上传后再传。`Bytes`（clone 是引用计数）和本地文件（能重新打开）都能重复读
- 流只能读一遍，所以 sha256、字节数、Content-Type、扩展名由调用方给，`put` 不读内容，跳过的话流不会被读；
  sha256、字节数作为 `x-amz-checksum-sha256`、`Content-Length` 带上，给错了服务端拒绝写入，`put` 不用信任调用方。
  考虑过 `put` 先把流落到临时文件再读两遍：要磁盘空间和临时文件的管理，调用方知道摘要时（比如源头是另一个 S3）用不着。
  流不能重放，上传中途失败 SDK 不重试
- 长度未知的流（别处传来的下载流）本来也上传不了：PutObject 要先知道长度，未知长度只能分片上传，还没做（见 todo）
- 扩展名跟着内容走：本地文件就是路径的扩展名，不另给；`Bytes` 没有名字，扩展名放在变体里，可以不带

**算完摘要之后内容被改：带上 sha256 让服务端校验**

本地文件在算完摘要、上传之前可能被改了，传上去的内容和拼 key 用的 sha256 就对不上。上传时带上 `x-amz-checksum-sha256`，
服务端按收到的内容校验，对不上拒绝写入，所以 key 上的对象一定是这个 sha256 的内容。

**Content-Type 由 `put` 定**

在 `put` 里按这次读的内容定，保证类型就是这次传的内容的（考虑过公开识别函数、调用方传进来：没法保证传的就是它算的）。
设在对象上，下载时服务直接返回，调用方要存就从 `PutDigest` 取

## 4. 读对象：`get` / `head` 不管 sha256，要的用 `*_with_sha256`

`get` 返回 `ObjectStream`（元数据 + 内容的字节流），`head` 返回 `ObjectMeta`，对象不存在都是 `None`。

- `get` 不把内容读进内存：读出来的常常是转给别处（HTTP 响应、文件、`S3Bucket::put_object` 写到另一个 S3），整段要的调 `bytes()`。
  考虑过返回 `Bytes`：大对象整个进内存，转发时用不着
- 除内容外的都在 `ObjectMeta` 里，`ObjectStream` 只有 `meta`、`body` 两项。字段按 hygiea 的概念起名：字节数叫 `size`，
  和 `PutDigest::size`、`put_object` 的参数一样，不叫响应头的 Content-Length。
  实际的 bucket 名、完整 key 也放在里面但不公开：读内容出错时带进错误、`Debug` 里有；公开的话调用方容易把完整 key 存下来，
  再传回带前缀的句柄，前缀加两遍（第 2 节）
- 内容的 sha256 分成两组方法：`get_with_sha256` / `head_with_sha256` 请求时带上 checksum mode，返回 `(…, String)`，
  对象上没有（上传时没带，不经 hygiea 传的）报 `S3Err::MissingSha256`；`get` / `head` 不要，返回里也没有这一项。
  考虑过 `ObjectMeta` 里放 `Option<String>`：一定要的地方还得自己处理 `None`；考虑过 bool 参数：调用处看不出 `true` 是什么，
  传了 `true` 拿到的还是 `Option`；考虑过请求构造器（`.require_sha256().await`）：要么返回类型不变、还是 `Option`，
  要么 `ObjectMeta` 带上泛型参数、再加一个 sealed trait。分成两个方法，返回类型直接写明有没有
- `get` / `head` 不打开 checksum mode：GetObject 上打开它，SDK 就会边读边按它校验内容，要不要校验由客户端配置
  `response_checksum_validation` 管（默认的 `when_supported` 下 SDK 自己会打开），hygiea 不替调用方打开。
  `get_with_sha256` 因此一定会校验；HeadObject 没有内容，打开只是多返回一个响应头
- 只看响应里的 `checksum_sha256`，不看 `checksum_type`：RustFS 单次上传的对象不返回 `checksum_type`，按 `FULL_OBJECT` 判断就拿不到；
  分片上传的对象给的是各片 sha256 的组合值（带 `-N`，`COMPOSITE`），不是内容的 sha256，hygiea 自己不分片上传，
  读别处分片传的对象时 `*_with_sha256` 拿到的是这个组合值（R2 不支持分片上传带 sha256，不会有）

## 5. 预签名 GET 给外部访问：按服务地址签名、签完再换地址，有效期的上限在 bucket 上

前端等外部访问的地址常常不是服务端连服务的地址：服务端在内网连 RustFS，前端经公网域名、反向代理访问；R2 不想暴露带账号 ID 的 S3 接口地址，经自己的域名、代理访问。
SigV4 的签名覆盖 Host、路径、query，服务端按它**自己收到的**请求验签。考虑过三种做法：

- 用公网地址签名（另建一个服务地址换成公网地址的 SDK 客户端，只用来签名），代理原样转发 Host：
  只对不按 Host 路由的服务（RustFS、MinIO）成立。R2、AWS 靠 Host 找到账号和 bucket，Host 是公网域名时路由不到；
  代理把 Host 改成服务的域名又和签名对不上
- 签名地址里带上分流用的路径（`https://x.com/s3`）：路径进了签名，代理切掉它就验签失败；不切的话 RustFS 把第一段当成 bucket 名
- 按服务地址签名，签完把 URL 的 `scheme://host[:port]` 换成 `presign_public_origin`、在路径前面插上 `presign_path_prefix`（现在的做法）：
  代理切掉路径前缀、把 Host 改回服务地址的 host，服务端收到的和签名时一模一样，所以哪种兼容 S3 的服务都能用。
  代价是代理要改 Host，而且要和服务地址一字不差（带端口）；换地址、插前缀只是改字符串，不需要第二个 SDK 客户端

`presign_public_origin` 只允许 `scheme://host[:port]`、不能带路径：要插的路径都在 `presign_path_prefix` 里，两项各管一件事，
不会出现「换掉的地址里带路径、又插了一段前缀」这种不知道该怎么拼的组合。
R2 绑在 bucket 上的自定义域名不接受 SigV4 签名，`presign_public_origin` 不能填它，只能填自己代理的地址；
想走自定义域名、CDN 缓存的是不签名的公开地址，还没做（见 todo）。

有效期的上限配在 bucket 上（`presign_max_expires_in`，不写是 SigV4 允许的 7 天），签名时不传就用它、传了的不能超过它：

- 放在 bucket 级：同一个服务上不同用途的 bucket（公开资源、私有文件）能给的最长时间不一样，按 bucket 管；
  调用方要更短的照样能传
- 一项同时当上限和默认值：只有默认值的话调用方传多长都行，管不住；只有上限的话每次调用都得自己再定一个值。
  调用方大多直接用上限，不用每次传
- 配置里的值启动时检查（超出 1 秒到 7 天是配置写错，报 `InvalidConfig`）；调用时传的超出范围是参数的问题，报 `S3Err::InvalidPresignExpiry`，
  在交给 SDK 之前检查，错误里带上 bucket 的上限

签进 URL 的 `response-*` 参数（`PresignGetOptions`）在读取时覆盖对象上存的响应头，拿到 URL 的人改不了：

- 下载时的文件名用 `PresignGetOptions::attachment` / `inline`，传原始的名字，拼成 RFC 6266 的 Content-Disposition：
  `filename` 是 ASCII 的后备名（`"`、`\`、控制字符、非 ASCII 字符换成 `_`），`filename*` 是按 RFC 5987 编码的完整名字（用 `percent-encoding`）。
  考虑过只留原始字段让调用方自己拼：非 ASCII 的名字不编码，浏览器会乱码或者丢掉名字，`"`、`\`、换行还要转义，每个调用方都得懂这些规则。
  字段仍然公开，要别的写法的自己填
- `presigned_get_url` 按值收选项：选项最后交给 SDK，SDK 要的是 `String`，按引用收就得在里面复制一遍

没有一次签一批的方法（考虑过 `presigned_get_urls(keys) -> key → URL`）：签名在本地算，一批也只是逐个签，省不下请求；
调用方手里常常是自己的 id，要的是 id → URL，按 key 返回还得再换回去，不如自己循环调 `presigned_get_url`

只有预签名 GET，没有预签名 PUT（给前端直传）：只签 key 和有效期的话，内容、类型、大小、会不会覆盖都管不了，传上来的对象没有 sha256，
key 也不是按内容拼的；能签进去的头（Content-Type、确切的大小、`If-None-Match`）要前端原样带上，返回值也得带上这些头，
没有具体的用法时定不下来，所以没留。
