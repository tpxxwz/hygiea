// 模块里没有带测试属性的函数，多半是忘了写 #[test] / #[tokio::test]
#[hygiea_test::container]
mod rustfs {
    fn put_get() {}
}

fn main() {}
