// lock_internal_module_prefix 只给 hygiea 自己的 crate（项目前缀 999）用
use hygiea::lock::hy_lock_type;

#[derive(hy_lock_type)]
#[lock_internal_module_prefix = "100"]
enum E {
    #[lock_type(code = "001")]
    A,
}

fn main() {}
