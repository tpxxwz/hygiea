// 模板里用内置函数（range）不算变量；非 ASCII 的 enum / 变体名；{ .. } 里多给的 key、表达式 key、嵌套值
use hygiea::{err, hy_err};

#[derive(hy_err)]
enum 用户错误 {
    #[error(err_code = "00101", err_tpl = "{% for i in range(n) %}{{ i }}{% endfor %}")]
    未找到,
    #[error(err_code = "00102", err_tpl = "{{ a }}/{{ b.c }}")]
    无权限,
}

fn main() {
    assert_eq!(err!(用户错误::未找到, 3).to_string(), "012");

    let e = err!(用户错误::无权限, { "a": 1, "b": { "c": [1, 2] }, "extra": "kept" });
    assert_eq!(e.to_string(), "1/[1, 2]");
    assert_eq!(e.err_args()["extra"], "kept");

    let key = "a";
    let e = err!(用户错误::无权限, { (key): 1, "b": { "c": 2 } });
    assert_eq!(e.to_string(), "1/2");
}
