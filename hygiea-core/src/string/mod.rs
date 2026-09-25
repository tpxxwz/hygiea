//! 字符串工具：正则（带编译缓存）和模板渲染。

mod pattern;
mod template;

pub use pattern::*;
pub use template::{tpl_cached, tpl_once, tpl_pos};
