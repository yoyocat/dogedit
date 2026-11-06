//! `dg-render` 的 token 名字必须落在 `dg-skin` 认识的区域（或区域族）里。
//!
//! 这个测试只能放在这里：`dg-render` 不能依赖 `dg-skin`（反过来才行），所以
//! 「名字前缀合法」这条约束由依赖方来守。
//!
//! 为什么重要：token 名字是 `<family>.<slot>`，皮肤里「哪个区域读哪些 token」是由
//! 区域决定的。前缀不认识的 token 永远匹配不到会读它的 painter —— 表现是
//! 「皮肤里写了，但没反应」，最难查。

use dg_render::tokens::TOKEN_NAMES;
use dg_skin::Region;

/// 区域族：一个名字管多个区域的颜色。
///
/// 这几个颜色是**共用**的，不属于某一个区域：`scroll.bg` 是两根滚动条共用的槽色，
/// `status.bg` 是状态栏 band（含它里面的格与尺寸柄），`menu.bg` 是整个弹出菜单。
/// 硬塞给某一个区域反而会让人以为另一个区域不读它。
const FAMILIES: &[&str] = &["scroll", "status", "menu"];

#[test]
fn every_token_starts_with_a_known_region() {
    for name in TOKEN_NAMES {
        let known = Region::ALL
            .iter()
            .map(|r| r.as_str())
            .chain(FAMILIES.iter().copied())
            .any(|prefix| {
                name.starts_with(prefix) && name[prefix.len()..].starts_with('.')
            });
        assert!(known, "{name} does not start with a known region or family");
    }
}

/// 族名不许和区域名撞车 —— 撞了就看不出 `menu.separator` 是区域还是 token。
#[test]
fn no_family_shadows_a_region() {
    for family in FAMILIES {
        assert!(
            !Region::ALL.iter().any(|r| r.as_str() == *family),
            "{family} is already a region"
        );
    }
}
