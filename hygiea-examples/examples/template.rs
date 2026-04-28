//! Template rendering example
//!
//! Run with:
//! ```bash
//! cargo run -p hygiea-examples --example template
//! ```
//!
//! 这不是 Rust format! 的替代品。format! 的模板字符串必须在编译期写死。
//! 这里的模板字符串可以在运行时从配置文件、数据库、API 等任意来源动态获取。
//!
//! 两套 API：
//! - `fmt_tpl` / `fmt_tpl_once`：命名参数，适合复杂模板（条件、循环、嵌套数据）
//! - `fmt_pos` / `fmt_pos_once`：位置参数，适合简单模板，不用写 key，不用写字段名

use hygiea::{fmt_pos, fmt_pos_once, fmt_tpl, fmt_tpl_once};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Hygiea Template Examples ===\n");

    // 模拟从数据库或配置文件读取的模板字符串
    let email_tpl = std::env::var("EMAIL_TEMPLATE").unwrap_or_else(|_| {
        "Dear {{ name }}, your order {{ order_id }} has been shipped.".to_string()
    });

    let sms_tpl = std::env::var("SMS_TEMPLATE")
        .unwrap_or_else(|_| "{% if urgent %}URGENT: {% endif %}{{ msg }}".to_string());

    let report_tpl = std::env::var("REPORT_TEMPLATE").unwrap_or_else(|_| {
        r#"Report for {{ title }}:
{% for item in items %}
- {{ item.name }}: {{ item.value }}
{% endfor %}
Total: {{ total }}"#
            .to_string()
    });

    let pos_tpl = std::env::var("POS_TEMPLATE")
        .unwrap_or_else(|_| "Hello {}, you have {} messages".to_string());

    // ===== fmt_tpl_once 示例 =====

    println!("1. fmt_tpl_once — 动态模板渲染:");
    let msg = fmt_tpl_once!(&email_tpl, {
        "name": "Alice",
        "order_id": "ORD-001"
    })?;
    println!("   {}\n", msg);

    println!("2. fmt_tpl_once — 条件渲染:");
    let msg = fmt_tpl_once!(&sms_tpl, {
        "urgent": true,
        "msg": "Server down"
    })?;
    println!("   {}\n", msg);

    // ===== fmt_tpl 示例 =====

    println!("3. fmt_tpl — 同一模板多次渲染 (命中缓存):");
    let report1 = fmt_tpl!(&report_tpl, {
        "title": "Sales",
        "items": [
            {"name": "Product A", "value": 100},
            {"name": "Product B", "value": 200}
        ],
        "total": 300
    })?;
    let report2 = fmt_tpl!(&report_tpl, {
        "title": "Sales",
        "items": [{"name": "Product C", "value": 500}],
        "total": 500
    })?;
    println!("{}\n", report1);
    println!("{}\n", report2);

    println!("4. fmt_tpl — 条件渲染:");
    let msg = fmt_tpl!(&sms_tpl, {
        "urgent": false,
        "msg": "All good"
    })?;
    println!("   {}\n", msg);

    // ===== fmt_pos_once 示例 =====

    println!("5. fmt_pos_once — 位置参数，不用写 key:");
    let msg = fmt_pos_once!(&pos_tpl, "Alice", 3)?;
    println!("   {}\n", msg);

    println!("6. fmt_pos_once — 动态模板，直接传值:");
    let pos_tpl2 =
        std::env::var("POS_TEMPLATE2").unwrap_or_else(|_| "Order {} belongs to {}".to_string());
    let msg = fmt_pos_once!(&pos_tpl2, "ORD-002", "Bob")?;
    println!("   {}\n", msg);

    // ===== fmt_pos 示例 =====

    println!("7. fmt_pos — 同一模板多次渲染 (命中缓存):");
    let greeting = std::env::var("GREETING").unwrap_or_else(|_| "Hi {}!".to_string());
    let first = fmt_pos!(&greeting, "Carol")?;
    let second = fmt_pos!(&greeting, "Dave")?;
    println!("   {}", first);
    println!("   {}\n", second);

    println!("8. fmt_pos — 转义字面量 {{}}:");
    // 模板中 {{ }} 是字面量 {}，不会被当占位符
    let tpl = "Score: {}%, result: {{}}";
    let msg = fmt_pos!(tpl, 95)?;
    println!("   {}\n", msg);

    Ok(())
}
