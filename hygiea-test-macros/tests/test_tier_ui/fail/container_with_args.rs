// #[container] 不收参数
#[hygiea_test::container(env = ["A"])]
mod rustfs {
    #[test]
    fn put_get() {}
}

fn main() {}
