//! 测试夹具 DLL：把 tree_sitter_json 的语言函数经本 crate 再导出。
//!
//! dg-syntax 的测试据此走真·LoadLibrary 链路（加载 → 取符号 → 调用 →
//! 校验），同时验证 manifest `entry` 指向非缺省符号的覆盖语义。
//!
//! 注意导出名**不能**叫 `tree_sitter_json`：grammar crate 的
//! `extern "C" { fn tree_sitter_json() }` 若与自家导出同名，链接器会把
//! extern 解析到本 DLL 自己的导出，形成自引用无限递归（STATUS_STACK_OVERFLOW）。
//! 故用 `tree_sitter_fixture_json`，由测试通过 manifest `entry` 指名。

/// 转发到 grammar crate 静态编入的 `tree_sitter_json`（parser.c 经其
/// build.rs 编进 rlib，符号在本 DLL 内有定义）。
#[unsafe(no_mangle)]
pub extern "C" fn tree_sitter_fixture_json() -> *const () {
    unsafe { tree_sitter_json::LANGUAGE.into_raw()() }
}
