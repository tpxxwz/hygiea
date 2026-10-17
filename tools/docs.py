#!/usr/bin/env python3
"""文档的辅助命令：只做结果确定的事（列目录、按代码生成表格、逐项比对），内容重不重复、写在哪合适交给人和 agent 判断。

用法（在哪个目录执行都行，只用标准库，macOS 自带的 python3 3.9 和 Ubuntu 24 的 3.12 都能跑）：

  tools/docs.py tree [路径..]   打印目录树，.rs 文件后面接它 //! 的第一段。不给路径时是整个仓库（不含 playground）
  tools/docs.py gen             重新生成 md 里 <!-- gen:xxx --> 和 <!-- /gen:xxx --> 之间的内容：
                                  README 的内置错误码表（hygiea/tests/err_code_table.rs，要编译全部 feature）
                                  hygiea-examples/README.md 的示例列表（[[example]] + 每个示例 //! 的第一段）
  tools/docs.py check           只检查，有问题列出来并返回 1（release.sh 发布前会跑）：
                                  生成的两处是不是最新的
                                  src/ 下的 .rs 文件是不是都有 //!
                                  README <!-- check:features --> 段表格第一列的 feature 和 hygiea/Cargo.toml 一一对应
                                  md 里反引号写的仓库路径存在、相对链接能打开。README「快速开始」一节写的是使用方项目里的路径，
                                  不检查；指第三方源码的路径不加反引号
                                  各 crate 的 config/*.toml 模板都列在 README 里
                                  AGENTS.md 里重新生成 trybuild 快照的命令覆盖了全部 *_ui 测试
"""

import json
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EXAMPLES_README = "hygiea-examples/README.md"
# 检查路径和链接的 md
CHECKED_MD_DIRS = ["docs/design", "docs/todo"]
CHECKED_MD = ["AGENTS.md", "README.md", EXAMPLES_README]


def read(path):
    with open(os.path.join(ROOT, path), encoding="utf-8") as f:
        return f.read()


def write(path, text):
    with open(os.path.join(ROOT, path), "w", encoding="utf-8") as f:
        f.write(text)


def run(cmd, **kw):
    return subprocess.run(cmd, cwd=ROOT, check=False, **kw)


def repo_files(paths=()):
    """仓库里的文件：已跟踪的加上没被忽略的新文件，不含 playground、target"""
    out = run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "--", *paths],
        stdout=subprocess.PIPE,
        text=True,
    ).stdout
    files = sorted(set(out.split()))
    return [
        f
        for f in files
        if not f.startswith(("playground/", "target/")) and os.path.exists(os.path.join(ROOT, f))
    ]


def first_doc(path):
    """//! 的第一段（到第一个空的 //! 为止），几行接成一行；`- ` 开头的列表项之间加空格"""
    out = ""
    with open(os.path.join(ROOT, path), encoding="utf-8") as f:
        for line in f:
            if not line.startswith("//!"):
                if out:
                    break
                continue
            text = re.sub(r"^//! ?", "", line.rstrip("\n"))
            if not text:
                if out:
                    break
                continue
            out = out + (" " if out and text.startswith("- ") else "") + text
    return out


_metadata = None


def metadata():
    global _metadata
    if _metadata is None:
        out = run(
            ["cargo", "metadata", "--no-deps", "--format-version", "1"],
            stdout=subprocess.PIPE,
            text=True,
        ).stdout
        _metadata = json.loads(out)
    return _metadata


def package(name):
    for p in metadata()["packages"]:
        if p["name"] == name:
            return p
    raise SystemExit("docs.py: cargo metadata 里没有 " + name)


# ---------------------------- tree ----------------------------


def cmd_tree(paths):
    prev = []
    for f in repo_files(paths):
        parts = f.split("/")
        dirs = parts[:-1]
        same = True
        for i, d in enumerate(dirs):
            if same and i < len(prev) and prev[i] == d:
                continue
            same = False
            print("  " * i + d + "/")
        prev = dirs
        line = "  " * len(dirs) + parts[-1]
        if f.endswith(".rs"):
            doc = first_doc(f)
            if doc:
                line += "  — " + doc
        print(line)


# ---------------------------- 生成 ----------------------------


def examples_table():
    rows = ["| 示例 | 讲什么 |", "|---|---|"]
    targets = [t for t in package("hygiea-examples")["targets"] if "example" in t["kind"]]
    for t in sorted(targets, key=lambda t: t["name"]):
        rel = os.path.relpath(t["src_path"], ROOT)
        doc = first_doc(rel) or "（{} 没有 //!）".format(rel)
        rows.append("| `{}` | {} |".format(t["name"], doc))
    return "\n".join(rows)


def replace_section(text, tag, body, path):
    begin, end = "<!-- gen:{} -->".format(tag), "<!-- /gen:{} -->".format(tag)
    if begin not in text or end not in text:
        raise SystemExit("docs.py: {} 里没有 {} / {} 标记".format(path, begin, end))
    i = text.index(begin) + len(begin)
    j = text.index(end)
    return text[:i] + "\n" + body + "\n" + text[j:]


def err_code_test(overwrite):
    env = dict(os.environ)
    if overwrite:
        env["HYGIEA_DOCS"] = "overwrite"
    cmd = ["cargo", "test", "-q", "-p", "hygiea", "--all-features", "--test", "err_code_table"]
    if overwrite:
        return run(cmd, env=env).returncode == 0
    return run(cmd, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0


def cmd_gen():
    text = read(EXAMPLES_README)
    write(EXAMPLES_README, replace_section(text, "examples", examples_table(), EXAMPLES_README))
    print("已生成 {} 的示例列表".format(EXAMPLES_README))
    if not err_code_test(overwrite=True):
        raise SystemExit("docs.py: 生成错误码表失败")
    print("已生成 README.md 的错误码表")


# ---------------------------- 检查 ----------------------------


def check_generated(problems):
    text = read(EXAMPLES_README)
    if replace_section(text, "examples", examples_table(), EXAMPLES_README) != text:
        problems.append("{} 的示例列表不是最新的，运行 tools/docs.py gen".format(EXAMPLES_README))
    if not err_code_test(overwrite=False):
        problems.append("README.md 的错误码表不是最新的，运行 tools/docs.py gen")


def check_module_docs(problems):
    missing = [
        f
        for f in repo_files()
        if f.endswith(".rs") and re.search(r"(^|/)src/", f) and not read(f).lstrip("﻿").startswith("//!")
        and "\n//!" not in read(f)
    ]
    for f in missing:
        problems.append("没有 //!（模块说明）：" + f)


def check_features(problems):
    readme = read("README.md")
    m = re.search(r"<!-- check:features -->(.*?)<!-- /check:features -->", readme, re.S)
    if not m:
        problems.append("README.md 里没有 <!-- check:features --> / <!-- /check:features --> 标记")
        return
    in_readme = set()
    for line in m.group(1).splitlines():
        if line.startswith("|"):
            cols = line.split("|")
            if len(cols) > 1:
                in_readme.update(re.findall(r"`([a-z][a-z0-9-]*)`", cols[1]))
    in_cargo = set(package("hygiea")["features"]) - {"default"}
    for f in sorted(in_cargo - in_readme):
        problems.append("feature 只在 hygiea/Cargo.toml、README 的表里没有：" + f)
    for f in sorted(in_readme - in_cargo):
        problems.append("feature 只在 README 的表里、hygiea/Cargo.toml 没有：" + f)


def checked_md():
    files = list(CHECKED_MD)
    for d in CHECKED_MD_DIRS:
        files += sorted(
            os.path.join(d, f) for f in os.listdir(os.path.join(ROOT, d)) if f.endswith(".md")
        )
    return files


# 反引号里像仓库路径的：带 `/`，以文件扩展名或 `/` 结尾，没有占位符、通配符、空格、`::`
PATH_TOKEN = re.compile(r"`([A-Za-z0-9_.\-/]+/[A-Za-z0-9_.\-]*(?:\.(?:rs|md|toml|sql|sh|py|lock)|/))`")


def path_exists(token, all_files, all_dirs):
    """仓库根下、`hygiea-components/` 下，或者某个文件 / 目录的路径以它结尾（写的是 crate 内的相对路径）"""
    token = token.rstrip("/") if token.endswith("/") else token
    for base in ("", "hygiea-components/"):
        if os.path.exists(os.path.join(ROOT, base + token)):
            return True
    suffix = "/" + token
    return any(f.endswith(suffix) for f in all_files) or any(d.endswith(suffix) for d in all_dirs)


def check_paths(problems):
    all_files = repo_files()
    all_dirs = set()
    for f in all_files:
        d = os.path.dirname(f)
        while d:
            all_dirs.add(d)
            d = os.path.dirname(d)
    for md in checked_md():
        text = read(md)
        # 代码块里的不算
        text = re.sub(r"```.*?```", "", text, flags=re.S)
        if md == "README.md":
            text = re.sub(r"\n## 快速开始\n.*?(?=\n## )", "\n", text, flags=re.S)
        for token in sorted(set(PATH_TOKEN.findall(text))):
            if token.startswith(("http", "/")) or "://" in token:
                continue
            if not path_exists(token, all_files, all_dirs):
                problems.append("{} 里写的路径不存在：{}".format(md, token))
        # 相对链接 [..](path)
        for target in sorted(set(re.findall(r"\]\(([^)\s]+)\)", text))):
            if re.match(r"^[a-z]+:", target) or target.startswith("#"):
                continue
            path = target.split("#", 1)[0]
            full = os.path.normpath(os.path.join(ROOT, os.path.dirname(md), path))
            if not os.path.exists(full):
                problems.append("{} 里的链接打不开：{}".format(md, target))


def check_config_templates(problems):
    readme = read("README.md")
    linked = set(re.findall(r"\]\(([^)\s]+\.toml)\)", readme))
    for f in repo_files(["hygiea-core", "hygiea-components"]):
        if re.search(r"/config/[^/]+\.toml$", f) and f not in linked:
            problems.append("配置模板没列在 README 的模板表里：" + f)


def check_trybuild(problems):
    agents = read("AGENTS.md")
    listed = set()
    for line in agents.splitlines():
        if "TRYBUILD=overwrite" not in line:
            continue
        m = re.search(r"-p\s+([\w-]+)", line)
        if m:
            for t in re.findall(r"--test\s+([\w-]+)", line):
                listed.add((m.group(1), t))
    actual = set()
    for p in metadata()["packages"]:
        for t in p["targets"]:
            if "test" in t["kind"] and t["name"].endswith("_ui"):
                actual.add((p["name"], t["name"]))
    for pkg, t in sorted(actual - listed):
        problems.append("AGENTS.md 的 trybuild 快照命令里漏了：-p {} --test {}".format(pkg, t))
    for pkg, t in sorted(listed - actual):
        problems.append("AGENTS.md 的 trybuild 快照命令里的测试不存在：-p {} --test {}".format(pkg, t))


def cmd_check():
    problems = []
    check_generated(problems)
    check_module_docs(problems)
    check_features(problems)
    check_paths(problems)
    check_config_templates(problems)
    check_trybuild(problems)
    if problems:
        for p in problems:
            print("✗ " + p)
        return 1
    print("✓ 文档检查通过")
    return 0


def main(argv):
    if len(argv) >= 2 and argv[1] == "tree":
        cmd_tree(argv[2:])
        return 0
    if len(argv) == 2 and argv[1] == "gen":
        cmd_gen()
        return 0
    if len(argv) == 2 and argv[1] == "check":
        return cmd_check()
    print(__doc__)
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
