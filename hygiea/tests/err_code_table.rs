//! README「保留和内置错误码」里的错误码表由这里生成：进程里注册的、`999` 开头的错误码，按码排序。
//!
//! 默认只检查 README 里的表是不是最新的，对不上就失败；`HYGIEA_DOCS=overwrite` 时重写那张表（`tools/docs.py gen` 会调用）。
//! 组件的错误要开了 feature 才会链接进来，所以要全开：
//!
//! ```bash
//! cargo nextest run -p hygiea --all-features --test err_code_table
//! HYGIEA_DOCS=overwrite cargo test -p hygiea --all-features --test err_code_table
//! ```

use hygiea::__private::ERR_REGISTRATIONS;

const BEGIN: &str = "<!-- gen:err-codes -->";
const END: &str = "<!-- /gen:err-codes -->";

fn table() -> String {
    let mut regs: Vec<_> = ERR_REGISTRATIONS
        .iter()
        .filter(|r| r.err_code.starts_with("999"))
        .collect();
    regs.sort_by_key(|r| r.err_code);
    let mut out = String::from("| 错误码 | 变体 | 模板 |\n|---|---|---|\n");
    for r in regs {
        let tpl = r.err_tpl.replace('|', "\\|").replace('\n', " ");
        out.push_str(&format!("| `{}` | `{}` | {} |\n", r.err_code, r.err_name, tpl));
    }
    out
}

#[test]
fn readme_err_code_table_is_current() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../README.md");
    let readme = std::fs::read_to_string(path).unwrap();
    let begin = readme.find(BEGIN).expect("README 里没有 <!-- gen:err-codes --> 标记") + BEGIN.len();
    let end = readme.find(END).expect("README 里没有 <!-- /gen:err-codes --> 标记");
    let expected = format!("\n{}", table());
    if readme[begin..end] == expected {
        return;
    }
    if std::env::var("HYGIEA_DOCS").as_deref() == Ok("overwrite") {
        let updated = format!("{}{}{}", &readme[..begin], expected, &readme[end..]);
        std::fs::write(path, updated).unwrap();
        return;
    }
    panic!(
        "README 的错误码表不是最新的，运行 tools/docs.py gen 重新生成。应该是：\n{}",
        table()
    );
}
