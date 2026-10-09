//! 调查：RustRover 在哪些写法上显示测试的运行按钮。
//! 每个模块一种写法，打开这个文件看各模块、各函数旁边有没有按钮。

/// A：不挂任何宏，只有内置的 #[ignore]
mod a_plain {
    #[test]
    #[ignore = "container"]
    fn plain_ignored() {}

    #[test]
    fn plain() {}
}

/// B：模块上挂第三方属性宏（serial_test 的 #[serial] 可以标在模块上）
#[serial_test::serial]
mod b_third_party_on_mod {
    #[test]
    fn under_serial_mod() {}
}

/// C：函数上挂第三方属性宏，加在 #[test] 之外
mod c_third_party_on_fn {
    #[test]
    #[serial_test::serial]
    fn serial_fn() {}
}

/// D：tokio::test 本身就是属性宏
mod d_tokio_test {
    #[tokio::test]
    async fn tokio_fn() {}
}

/// E：我们的 #[container]（旧版：挪进层名子模块，内外两层固定加 #[cfg(test)]）
#[hygiea_test::container]
mod e_ours_container {
    #[test]
    fn under_container() {}
}

/// F：第三方属性宏标在模块上 + async 辅助函数 + #[tokio::test]
#[serial_test::serial]
mod f_serial_async {
    async fn helper() -> i32 {
        1
    }

    #[tokio::test]
    async fn async_helper() {
        let _ = helper().await;
    }
}

/// G：我们的 #[container] + async 辅助函数 + #[tokio::test]（在 hygiea-redis 里没按钮的组合）
#[hygiea_test::container]
mod g_ours_async {
    async fn helper() -> i32 {
        1
    }

    #[tokio::test]
    async fn async_helper() {
        let _ = helper().await;
    }
}

/// H：模块不挂宏，第三方属性宏挂在测试函数上 + async 辅助函数 + #[tokio::test]
mod h_serial_on_fn_async {
    async fn helper() -> i32 {
        1
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn async_helper() {
        let _ = helper().await;
    }
}
