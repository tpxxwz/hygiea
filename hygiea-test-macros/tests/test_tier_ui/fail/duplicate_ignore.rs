// 宏已经给测试加了 #[ignore]，再写一个报错；嵌套模块里的也查
#[hygiea_test::container]
mod rustfs_container {
    mod nested {
        #[test]
        #[ignore]
        fn put_get() {}
    }
}

fn main() {}
