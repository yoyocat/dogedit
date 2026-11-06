//! 行号栏的右键菜单：三条显示行、一条分隔线，然后四个子菜单。
//!
//! 行序、分隔线的位置和子菜单的嵌套照菜单模板；每行的命令 id 和 `查看` 菜单里
//! 那几行是同一批，所以标题只写中文兜底，语言表按 id 覆盖 —— 这张菜单没有自己的
//! 小节，读的就是菜单栏那张表。子菜单的标题按「第一个孩子的 id」查，
//! `P1330` / `P1305` / `P1276` / `P1280` 四条正是 `查看` 里那四个弹层的标题。
//!
//! 模板不带状态：勾在弹出之前现算，问的是菜单栏同一张状态表
//! （[`menubar::check_of`]），所以这一行的勾和 `查看` 里同一条命令不会各说各话。
//! 只有跑不动的命令行才置灰，和菜单栏一个规矩。

use dg_ui::menu::MenuItem;

/// 这张菜单在帧里的编号：`request_popup` 拿它找行，`dispatch_menu_command` 拿它
/// 认行是从哪儿来的。它不对应语言表里的任何小节 —— 标题全走菜单栏那张表 ——
/// 所以这个数只是帧自己给这层弹层起的标签。
pub(super) const GUTTER_MENU: u32 = 400;

/// 切换书签：这张菜单里唯一按**打开菜单那一行**办事的行，别的书签行都跟着光标走。
/// 见 [`crate::win32::window::EditorState::bookmark_toggle_at`]。
pub(super) const TOGGLE_BOOKMARK: u32 = 1276;

/// 菜单的行。`check` 是菜单栏那张状态表（勾），`live` 说这一行的命令这实现能不能
/// 真做（灰）。弹层父行不是命令行，随时可以打开 —— 点开才看得见里面有什么，所以
/// 灰只落在命令行上。
pub(super) fn items(check: &dyn Fn(u32) -> bool, live: &dyn Fn(u32) -> bool) -> Vec<MenuItem> {
    let row = |id: u32, label: &str| {
        MenuItem::new(id, label)
            .with_checked(check(id))
            .with_enabled(live(id))
    };
    vec![
        row(1300, "行号(&L)"),
        row(1301, "标尺(&R)"),
        row(1287, "文档地图(&I)"),
        MenuItem::separator(),
        MenuItem::popup(
            1330,
            "代码折叠(&F)",
            vec![
                row(1330, "取消折叠(&C)"),
                MenuItem::separator(),
                row(1331, "语法折叠(&S)"),
                row(1332, "缩进折叠(&I)"),
                MenuItem::separator(),
                row(62, "折叠当前级别"),
                row(52, "展开当前级别"),
                row(1339, "折叠全部(&F)"),
                row(1340, "展开全部(&U)"),
                MenuItem::separator(),
                row(1622, "折叠级别:2"),
                row(1623, "折叠级别:3"),
                row(1624, "折叠级别:4"),
                row(1625, "折叠级别:5"),
                row(1626, "折叠级别:6"),
                row(1627, "折叠级别:7"),
                row(1628, "折叠级别:8"),
                row(1629, "折叠级别:9"),
            ],
        ),
        MenuItem::popup(
            1305,
            "符号显示(&S)",
            vec![
                row(1305, "当前行"),
                row(1307, "缩进线"),
                row(1311, "边界线"),
                row(1337, "列标记"),
                row(1672, "图标区域"),
                MenuItem::separator(),
                row(1302, "空白字符"),
                row(1303, "换行符"),
                row(1304, "文档结束符"),
                row(1306, "控制字符"),
                MenuItem::separator(),
                row(1310, "选择时全部显示"),
                row(1309, "折叠尾部字符"),
                row(1673, "扩展当前行背景"),
                row(1674, "同层背景色"),
                row(1675, "行修改状态"),
                row(1676, "显示书签"),
            ],
        ),
        MenuItem::popup(
            1276,
            "书签(&M)",
            vec![
                row(TOGGLE_BOOKMARK, "切换书签(&T)"),
                row(1279, "下一个书签(&N)"),
                row(1278, "上一个书签(&P)"),
                row(1277, "删除全部书签(&D)"),
                MenuItem::separator(),
                row(90, "剪切标记行(&M)"),
                row(89, "复制标记行(&L)"),
                MenuItem::separator(),
                row(1289, "书签管理器"),
                row(1676, "显示书签"),
            ],
        ),
        MenuItem::popup(
            1280,
            "图标提示",
            vec![
                row(1280, "下一个提示(&N)"),
                row(1281, "上一个提示(&P)"),
                row(1282, "清除所有提示(&C)"),
            ],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 顶层行压成 `id:标题`，弹层记成 `标题>`，分隔线记成 `-`。
    fn top_row(items: &[MenuItem]) -> Vec<String> {
        items
            .iter()
            .map(|it| {
                if it.separator {
                    "-".to_string()
                } else if it.children.is_empty() {
                    format!("{}:{}", it.id, it.label)
                } else {
                    format!("{}>", it.label)
                }
            })
            .collect()
    }

    #[test]
    fn the_top_rows_are_the_gutter_menu_in_the_bar_order() {
        // 和 `查看` 菜单同一批命令，但顺序是这张自己的：行号、标尺、文档地图，
        // 然后代码折叠在符号显示前面 —— 两处各按各的模板排，不许互相抄。
        let yes = |_: u32| true;
        assert_eq!(
            top_row(&items(&yes, &yes)),
            vec![
                "1300:行号(&L)",
                "1301:标尺(&R)",
                "1287:文档地图(&I)",
                "-",
                "代码折叠(&F)>",
                "符号显示(&S)>",
                "书签(&M)>",
                "图标提示>",
            ]
        );
    }

    /// 整张菜单压平：`id` 序列 + 分隔线 + 弹层深度，就是模板的那张表。
    fn flat(items: &[MenuItem], depth: usize, out: &mut Vec<String>) {
        for it in items {
            if it.separator {
                out.push(format!("{}-", "  ".repeat(depth)));
            } else if it.children.is_empty() {
                out.push(format!("{}{id}", "  ".repeat(depth), id = it.id));
            } else {
                out.push(format!("{}{}>", "  ".repeat(depth), it.label));
                flat(&it.children, depth + 1, out);
            }
        }
    }

    #[test]
    fn the_whole_menu_is_the_row_list_the_template_carries() {
        let yes = |_: u32| true;
        let mut out = Vec::new();
        flat(&items(&yes, &yes), 0, &mut out);
        assert_eq!(
            out,
            vec![
                "1300",
                "1301",
                "1287",
                "-",
                "代码折叠(&F)>",
                "  1330",
                "  -",
                "  1331",
                "  1332",
                "  -",
                "  62",
                "  52",
                "  1339",
                "  1340",
                "  -",
                "  1622",
                "  1623",
                "  1624",
                "  1625",
                "  1626",
                "  1627",
                "  1628",
                "  1629",
                "符号显示(&S)>",
                "  1305",
                "  1307",
                "  1311",
                "  1337",
                "  1672",
                "  -",
                "  1302",
                "  1303",
                "  1304",
                "  1306",
                "  -",
                "  1310",
                "  1309",
                "  1673",
                "  1674",
                "  1675",
                "  1676",
                "书签(&M)>",
                "  1276",
                "  1279",
                "  1278",
                "  1277",
                "  -",
                "  90",
                "  89",
                "  -",
                "  1289",
                "  1676",
                "图标提示>",
                "  1280",
                "  1281",
                "  1282",
            ]
        );
    }

    #[test]
    fn each_popup_parent_is_keyed_on_its_first_child() {
        // 语言表用 `P<第一个孩子的 id>` 存弹层标题，父行的 id 必须是第一个孩子的
        // id，否则换语言时这四行会掉回中文兜底。
        let yes = |_: u32| true;
        for p in items(&yes, &yes).iter().filter(|i| !i.children.is_empty()) {
            let first = p.children.iter().find(|c| !c.separator).map(|c| c.id);
            assert_eq!(Some(p.id), first, "弹层 {:?}", p.label);
        }
    }

    #[test]
    fn the_marks_come_from_the_state_table() {
        let check = |id: u32| id == 1300 || id == 1331;
        let yes = |_: u32| true;
        let items = items(&check, &yes);
        assert!(items[0].checked);
        assert!(!items[1].checked);
        let folding = &items[4].children;
        assert!(folding[2].checked, "语法折叠");
        assert!(!folding[0].checked, "取消折叠");
    }

    #[test]
    fn a_row_with_no_command_stays_visible_and_greyed() {
        // 行不能因为跑不动就消失 —— 下面的行会整体上移，和模板就对不齐了。
        let yes = |_: u32| true;
        let no = |_: u32| false;
        let items = items(&yes, &no);
        assert!(!items[0].enabled);
        assert!(items[0].checked);
        for p in items.iter().filter(|i| !i.children.is_empty()) {
            assert!(p.enabled, "弹层 {:?} 不是命令行，不该灰", p.label);
            assert!(p.children.iter().filter(|c| !c.separator).all(|c| !c.enabled));
        }
    }

    #[test]
    fn a_popup_does_not_list_the_same_row_twice() {
        // 显示书签 (1676) 在符号显示和书签两个弹层里各出现一次，这是模板的样子；
        // 同一个弹层里不许重复。
        let yes = |_: u32| true;
        for p in items(&yes, &yes).iter().filter(|i| !i.children.is_empty()) {
            let mut ids: Vec<u32> = p
                .children
                .iter()
                .filter(|c| !c.separator)
                .map(|c| c.id)
                .collect();
            let before = ids.len();
            ids.sort();
            ids.dedup();
            assert_eq!(ids.len(), before, "弹层 {:?} 里有重复行", p.label);
        }
    }
}
