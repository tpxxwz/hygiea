#!/usr/bin/env bash
# 发版：升版本号 → 改 README 里的版本号 → 提交 → 发布到 crates.io → 打 tag → push
#
# 用法（在哪个目录执行都行）：
#   release/release.sh 0.1.1-alpha.6        指定版本号
#   release/release.sh --bump alpha         按级别升：major / minor / patch / release / rc / beta / alpha
#   release/release.sh --dry-run --bump alpha
#                                           只检查：升版本和发布都走 dry-run，不改文件、不提交、不发布
#   release/release.sh --resume             上次发布到一半中断（网络断了、手动 Ctrl-C 等）时，
#                                           把 crates.io 上还没有的 crate 发完，再打 tag、push
#
# 限流：crates.io 限制短时间内新建 crate 的数量（首次发布一次性有好几个新 crate 时会碰到），
# 被限流（HTTP 429）时脚本自己等 RELEASE_RETRY_WAIT_SECS 秒（默认 600）再接着发，
# 已经发出去的跳过，直到全部发完；其他错误直接退出。等待期间可以 Ctrl-C，之后用 --resume 接着发。
#
# 发布用的 target 目录放在项目外（RELEASE_TARGET_DIR，默认 $TMPDIR/hygiea-release-target，下次发版复用，
# 不用每次从头编译）：cargo publish 验证时会检查 target/package/ 下解压出来的源码有没有被改动，
# 放在项目里的话，Finder / IDE 浏览目录时生成的 .DS_Store 会让验证失败。
# 需要：cargo-edit（提供 cargo set-version）、Cargo 1.90+（cargo publish --workspace）、jq、perl
#
# 发布顺序由 cargo publish --workspace 按依赖关系排，dev-dependency 也算在内，
# 所以组件可以带版本号依赖 hygiea-test。publish = false 的成员（test-support、hygiea-examples）不发布。
set -euo pipefail

cd "$(dirname "$0")/.."

# 只影响本脚本里的 cargo 调用
export CARGO_TARGET_DIR="${RELEASE_TARGET_DIR:-${TMPDIR:-/tmp}/hygiea-release-target}"

die() {
    echo "release: $*" >&2
    exit 1
}

dry_run=false
resume=false
bump=""
version=""
while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) dry_run=true ;;
        --resume) resume=true ;;
        --bump)
            [ $# -ge 2 ] || die "--bump needs a level"
            bump="$2"
            shift
            ;;
        -*) die "unknown option: $1" ;;
        *) version="$1" ;;
    esac
    shift
done

# 当前 workspace 的版本号，以 facade 为准
current_version() {
    local id
    id=$(cargo pkgid -p hygiea)
    echo "${id##*[#@]}"
}

# 要发布的成员（排除 publish = false 的），每行一个名字
publishable() {
    cargo metadata --no-deps --format-version 1 |
        jq -r '.packages[] | select(.publish != []) | .name'
}

# crates.io 上已经有 $1 这个版本的成员，输出成 `--exclude 名字` 参数
already_published_excludes() {
    local v="$1" name
    while read -r name; do
        if cargo info --registry crates-io "$name@$v" >/dev/null 2>&1; then
            printf -- '--exclude\n%s\n' "$name"
        fi
    done < <(publishable)
}

# 发布 crates.io 上还没有的成员；被限流就等一会儿再发剩下的，其他错误直接退出
publish_remaining() {
    local v="$1" wait="${RELEASE_RETRY_WAIT_SECS:-600}" log excludes
    log=$(mktemp)
    while true; do
        excludes=()
        while read -r arg; do excludes+=("$arg"); done < <(already_published_excludes "$v")
        echo "release: publishing $v, already on crates.io: $((${#excludes[@]} / 2)) crate(s)"
        if cargo publish --workspace "${excludes[@]}" 2>&1 | tee "$log"; then
            rm -f "$log"
            return 0
        fi
        if ! grep -qiE '429|too many requests|too many new crates|rate limit' "$log"; then
            rm -f "$log"
            die "cargo publish failed (not a rate limit), fix it and run with --resume"
        fi
        echo "release: rate limited by crates.io, retrying in ${wait}s (Ctrl-C and --resume later is fine)"
        sleep "$wait"
    done
}

tag_and_push() {
    local v="$1"
    git tag "$v"
    git push
    git push origin "$v"
    echo "release: $v published, tagged and pushed"
}

if $resume; then
    [ -z "$(git status --porcelain)" ] || die "working tree is not clean"
    v=$(current_version)
    publish_remaining "$v"
    tag_and_push "$v"
    exit 0
fi

if [ -n "$bump" ] && [ -n "$version" ]; then
    die "give either a version or --bump, not both"
fi
if [ -z "$bump" ] && [ -z "$version" ]; then
    die "missing version, see the usage at the top of release/release.sh"
fi
[ -z "$(git status --porcelain)" ] || die "working tree is not clean"

set_version_args=(--workspace)
if [ -n "$bump" ]; then
    set_version_args+=(--bump "$bump")
else
    set_version_args+=("$version")
fi

if $dry_run; then
    cargo set-version "${set_version_args[@]}" --dry-run
    cargo publish --workspace --dry-run
    echo "release: dry run done, nothing changed"
    exit 0
fi

# 1. 升版本号：workspace 版本和 [workspace.dependencies] 里内部 crate 的版本要求一起改
cargo set-version "${set_version_args[@]}"
v=$(current_version)
git tag --list "$v" | grep -q . && die "tag $v already exists"

# 2. README 里给使用方看的版本号
perl -pi -e "
    s/hygiea = \\{ version = \"[^\"]+\"/hygiea = { version = \"$v\"/g;
    s/hygiea = \"[0-9][^\"]*\"/hygiea = \"$v\"/g;
    s/hygiea\\@[0-9][^ ]*/hygiea\\@$v/g;
" README.md

# 3. 提交后再发布：发出去的内容和这个提交一致
git commit -am "chore: release $v"

# 4. 发布。被限流会自动等待重试；其他错误退出，已经发出去的不受影响，修好后用 --resume 接着发
publish_remaining "$v"

# 5. 全部发完才打 tag、push
tag_and_push "$v"
