//! The editor context menu: 19 visible rows plus the deferred tear-off. Row order,
//! separators, ids and popup nesting are the table below; captions come from the
//! language table (one section is shared by every menu, the editor popup's own
//! section overrides six rows) and accelerators from `key.ini`. Rows carry
//! no state, so greying and checking are applied at runtime. The tear-off row would
//! detach into a floating window and is not implemented yet, but stays visible and
//! disabled so the rows below it do not move.

use crate::keys::Command;
use dg_ui::menu::MenuItem;

/// The editor popup's own caption section, which is the key the frame's menu
/// seam uses to ask for this menu's rows.
pub(super) const EDIT_MENU: u32 = 304;

/// 撕下（tear-off）那一行，本实现还没有做；命名是为了下面的测试能证明
/// 表格从第 2 行开始。
const TEAR_OFF: u32 = 144;

/// 删除重复行 has two ids: 1452 keeps the topmost copy, 1409 the bottom one.
const DUP_LINES_BOTTOM: u32 = 1409;

/// Rows the menu shows and this implementation cannot yet perform, each for a feature
/// still owed: the tear-off window (row 1, so it does not appear in the table at
/// all). 全屏显示 left it once the full-screen toggle became the frame's own.
/// Greying is derived from this one list rather than repeated on each row, so a row
/// cannot be both dispatched and greyed: `every_leaf_row_is_accounted_for` fails if the
/// two tables ever overlap.
const DEFERRED: &[u32] = &[TEAR_OFF];

/// Rows whose state is a function of the selection: exactly these two are
/// `MF_GRAYED | MF_DISABLED` (`0x3`) on an empty selection, and every other row is
/// `0x0` in both states.
const SELECTION_ROWS: &[u32] = &[57635, 57634];

fn leaf(id: u32, label: &str) -> MenuItem {
    MenuItem::new(id, label)
}

/// Sets each **command** row's `enabled` from `DEFERRED` and `SELECTION_ROWS`.
///
/// A submenu parent is not a command row and is never inert — choosing it opens
/// it, which is exactly how the user finds out that 自定义标记 exists but has no
/// rows this implementation can perform yet. Greying a container would hide that, and in
/// the menu bar it would dim a top-level row `build_bar` never meant to grey.
fn apply_state(items: &mut [MenuItem], has_selection: bool) {
    for item in items {
        if item.separator {
            continue;
        }
        if !item.children.is_empty() {
            apply_state(&mut item.children, has_selection);
        } else {
            item.enabled = !DEFERRED.contains(&item.id)
                && (has_selection || !SELECTION_ROWS.contains(&item.id));
        }
    }
}

/// Its default folding mode is indent-based. This implementation has one folding mode, so no row
/// can claim a choice it does not offer.
pub(super) fn items(has_selection: bool, large: bool) -> Vec<MenuItem> {
    let mut items = vec![
        leaf(1338, "列模式(&C)"),
        MenuItem::separator(),
        leaf(57635, "剪切(&T)\tCtrl+X"),
        leaf(57634, "复制(&C)\tCtrl+C"),
        leaf(57637, "粘贴(&P)\tShift+Ins"),
        MenuItem::popup(
            91,
            "更多粘贴(&M)...",
            vec![
                leaf(91, "列模式粘贴(&P)\tCtrl+Shift+V"),
                leaf(94, "行尾列模式粘贴(&T)"),
                leaf(61, "缩进粘贴(保持缩进格式)(&I)"),
                leaf(1695, "从剪贴板历史中粘贴(&H)\tCtrl+K→Ctrl+V"),
                leaf(112, "粘贴\\0字符"),
            ],
        ),
        MenuItem::separator(),
        leaf(57642, "全选(&A)\tCtrl+A"),
        leaf(26, "删除(&D)\tBackspace"),
        MenuItem::popup(
            1428,
            "删除更多(&M)...",
            vec![
                leaf(1428, "删除空白行"),
                leaf(1452, "删除重复行(从上往下)"),
                leaf(DUP_LINES_BOTTOM, "删除重复行(从下往上)"),
                MenuItem::separator(),
                leaf(63, "删除行头空白(&L)"),
                leaf(64, "删除行尾空白(T)"),
                MenuItem::separator(),
                leaf(85, "删除单词(&W)"),
                leaf(28, "向左删除单词(&P)"),
                leaf(29, "向右删除单词(&N)"),
                leaf(30, "删除整行(&L)"),
                leaf(79, "删除到行头(&H)"),
                leaf(80, "删除到行尾(&E)"),
            ],
        ),
        MenuItem::separator(),
        MenuItem::popup(
            1461,
            "自定义标记(&C)",
            (0..8)
                .map(|i| leaf(1461 + i, &format!("使用标记{}", i + 1)))
                .chain([
                    MenuItem::separator(),
                    leaf(1318, "自定义标记(&C)"),
                    leaf(1471, "开关自定义标记"),
                ])
                .collect(),
        ),
        MenuItem::separator(),
        MenuItem::popup(
            1330,
            "代码折叠(&F)",
            vec![
                leaf(1330, "取消折叠(&C)"),
                MenuItem::separator(),
                leaf(1331, "语法折叠(&S)"),
                leaf(1332, "缩进折叠(&I)"),
                MenuItem::separator(),
                leaf(62, "折叠当前级别\tCtrl+K→Ctrl+["),
                leaf(52, "展开当前级别\tCtrl+K→Ctrl+]"),
                leaf(1339, "折叠全部(&F)\tCtrl+K→Ctrl+1"),
                leaf(1340, "展开全部(&U)\tCtrl+K→Ctrl+J"),
                MenuItem::separator(),
                leaf(1622, "折叠级别:2\tCtrl+K→Ctrl+2"),
                leaf(1623, "折叠级别:3\tCtrl+K→Ctrl+3"),
                leaf(1624, "折叠级别:4\tCtrl+K→Ctrl+4"),
                leaf(1625, "折叠级别:5\tCtrl+K→Ctrl+5"),
                leaf(1626, "折叠级别:6\tCtrl+K→Ctrl+6"),
                leaf(1627, "折叠级别:7\tCtrl+K→Ctrl+7"),
                leaf(1628, "折叠级别:8\tCtrl+K→Ctrl+8"),
                leaf(1629, "折叠级别:9\tCtrl+K→Ctrl+9"),
            ],
        ),
        MenuItem::popup(
            102,
            "转换(&C)",
            vec![
                leaf(102, "单词首字母大写(&C)"),
                leaf(57, "转为大写(&U)"),
                leaf(58, "转为小写(&L)"),
                leaf(82, "反转大小写(&I)"),
                MenuItem::separator(),
                leaf(97, "转为全角(&F)"),
                leaf(98, "转为半角(&H)"),
                MenuItem::separator(),
                leaf(69, "制表符转为空格(&T)"),
                leaf(70, "空格转为制表符(&S)"),
                leaf(83, "行头制表符转为空格(&L)"),
                leaf(84, "行头空格转为制表符(&L)"),
            ],
        ),
        MenuItem::popup(
            72,
            "更多编辑(&O)...",
            vec![
                leaf(72, "合并行(&J)"),
                MenuItem::separator(),
                leaf(47, "增加缩进(&I)"),
                leaf(48, "减少缩进(&D)"),
                leaf(51, "格式化缩进(&F)"),
                MenuItem::separator(),
                leaf(1413, "单行注释(&C)"),
                leaf(1414, "取消单行注释(&U)"),
                leaf(1415, "多行注释(&B)"),
                leaf(1416, "取消多行注释(&B)"),
            ],
        ),
        MenuItem::popup(
            1417,
            "时间和日期(&D)",
            vec![
                leaf(1417, "日期(&D)"),
                leaf(1418, "时间(&T)"),
                leaf(1419, "自定义日期/时间"),
            ],
        ),
        MenuItem::separator(),
        leaf(1328, "全屏显示(&F)\tF11"),
    ];
    if large {
        // 大文件模式专属段（本实现扩展，plan §4.6）：普通文档不出现，
        // 右键菜单是按当前活动文档状态生成的，所以不影响行为对照。
        items.push(MenuItem::separator());
        items.push(leaf(36001, "导出选区(&E)..."));
        items.push(leaf(36003, "替换结果导出(&R)..."));
        items.push(leaf(36002, "转为普通模式(&N)"));
        items.push(leaf(36004, "尾随日志(&T)"));
    }
    apply_state(&mut items, has_selection);
    items
}

/// The command a chosen row dispatches. This implementation goes through the same command layer the
/// keyboard uses, so a row and its shortcut cannot drift apart. `None` is a row this implementation
/// shows but does not implement yet.
pub(super) fn command_for(id: u32) -> Option<Command> {
    Some(match id {
        // 大文件模式段（本实现扩展 36000+；普通文档不会出现这些行）。
        36001 => Command::LargeExportSel,
        36002 => Command::LargeToNormal,
        36003 => Command::LargeReplaceExport,
        36004 => Command::LargeTail,
        57635 => Command::Cut,
        57634 => Command::Copy,
        57637 => Command::Paste,
        91 => Command::PasteColumn,
        57642 => Command::SelectAll,
        94 => Command::PasteAtLineTail,
        61 => Command::PasteAndIndent,
        112 => Command::PasteWithNul,
        1695 => Command::PasteHistory,
        26 => Command::DeletePrev,
        1428 => Command::DeleteBlankLines,
        1452 => Command::DeleteDupLinesTop,
        DUP_LINES_BOTTOM => Command::DeleteDupLinesBottom,
        63 => Command::TrimLeadingSpace,
        64 => Command::TrimTailingSpace,
        28 => Command::DeletePrevWord,
        29 => Command::DeleteNextWord,
        30 => Command::DeleteLine,
        79 => Command::DeleteToHeadOfLine,
        80 => Command::DeleteToEndOfLine,
        // "Delete current word" — the same operation the Ctrl+Del binding runs,
        // on the word the caret sits in.
        85 => Command::DeleteNextWord,
        97 => Command::ToFullWidth,
        98 => Command::ToHalfWidth,
        1417 => Command::InsertDate,
        1418 => Command::InsertTime,
        1419 => Command::CustomDateTime,
        // 自定义标记: 1461..1468 hand the selection text to slot N of the
        // process-wide marker set (the marker store), 1318 opens its manager
        // dialog, 1471 flips the whole set on or off (the state field).
        1461..=1468 => Command::MarkUse((id - 1461) as usize),
        1318 => Command::MarkDialog,
        1471 => Command::MarkToggle,
        // The Code Folding submenu, in the order the popup's own captions
        // list it. Its first three rows are the `FoldingMethod` radio group
        // (0/1/2), not toggles.
        1330 => Command::FoldCancel,
        1331 => Command::FoldBySyntax,
        1332 => Command::FoldByIndent,
        62 => Command::FoldCurrent,
        52 => Command::UnfoldCurrent,
        1339 => Command::FoldFoldAll,
        1340 => Command::FoldExpandAll,
        // Fold Level 2 … 9: the ids run consecutively and each names its own level.
        1622 => Command::FoldToLevel(2),
        1623 => Command::FoldToLevel(3),
        1624 => Command::FoldToLevel(4),
        1625 => Command::FoldToLevel(5),
        1626 => Command::FoldToLevel(6),
        1627 => Command::FoldToLevel(7),
        1628 => Command::FoldToLevel(8),
        1629 => Command::FoldToLevel(9),
        102 => Command::CapitalizeWords,
        // Column Mode: a latch on the editor control rather than a one-shot, so the
        // row turns the rectangle selection on and off and the movement commands
        // read it while it is set.
        1338 => Command::ColumnMode,
        57 => Command::ToUpper,
        58 => Command::ToLower,
        82 => Command::InvertCase,
        1413 => Command::LineComment,
        1414 => Command::LineUncomment,
        1415 => Command::BlockComment,
        1416 => Command::BlockUncomment,
        69 => Command::TabsToSpaces,
        70 => Command::SpacesToTabs,
        83 => Command::LeadingTabsToSpaces,
        84 => Command::LeadingSpacesToTabs,
        72 => Command::JoinLines,
        47 => Command::IncreaseIndent,
        48 => Command::DecreaseIndent,
        51 => Command::FormatIndent,
        // 全屏显示 ( F11): the frame toggle, the full-screen toggle.
        1328 => Command::ViewFullscreen,
        _ => return None,
    })
}

/// Every leaf id in the table, flattened — the test side of the fidelity check
/// below, since a row that never reaches `command_for` is invisible either way.
#[cfg(test)]
fn leaf_ids(items: &[MenuItem]) -> Vec<u32> {
    items
        .iter()
        .flat_map(|item| {
            let children = leaf_ids(&item.children);
            if item.separator {
                children
            } else if children.is_empty() {
                vec![item.id]
            } else {
                children
            }
        })
        .collect()
}

/// The ids this implementation shows but does not dispatch are the `DEFERRED` list at the
/// top of this file; anything visible that is in neither table is an oversight,
/// which is what `every_leaf_row_is_accounted_for` fails on.

#[cfg(test)]
mod tests {
    use super::*;

    /// The top level, position by position, against the measurements
    /// minus the tear-off row. A separator is `(0, true)`; a popup parent is
    /// listed by the id it shows with, which is its first child's id.
    #[test]
    fn menu_is_the_template_order_with_the_tear_off_deferred() {
        let items = items(false, false);
        let top: Vec<(u32, bool)> = items
            .iter()
            .map(|i| (if i.separator { 0 } else { i.id }, i.separator))
            .collect();
        assert_eq!(
            top,
            vec![
                (1338, false),
                (0, true),
                (57635, false),
                (57634, false),
                (57637, false),
                (91, false),
                (0, true),
                (57642, false),
                (26, false),
                (1428, false),
                (0, true),
                (1461, false),
                (0, true),
                (1330, false),
                (102, false),
                (72, false),
                (1417, false),
                (0, true),
                (1328, false),
            ]
        );
    }

    /// The live menu reported seven popup parents; `items()` has to keep all of
    /// them, since the earlier `MN_GETHMENU` walk that read four of them as
    /// separators was the reader's bug
    #[test]
    fn popup_parents_match_the_template() {
        let items = items(false, false);
        let popups: Vec<&str> = items
            .iter()
            .filter(|i| !i.children.is_empty())
            .map(|i| i.label.as_str())
            .collect();
        assert_eq!(
            popups,
            vec![
                "更多粘贴(&M)...",
                "删除更多(&M)...",
                "自定义标记(&C)",
                "代码折叠(&F)",
                "转换(&C)",
                "更多编辑(&O)...",
                "时间和日期(&D)",
            ]
        );
    }

    /// A command row is enabled exactly when `command_for` dispatches it and, for
    /// the two selection rows, when there is something to act on — so no row can
    /// look live and do nothing. The live menu measured `0x3` (grayed) for 剪切
    /// and 复制 with an empty selection.
    ///
    /// A submenu parent is always enabled: choosing it opens it, and an all-grey
    /// 自定义标记 is the honest way to show a feature exists but is not implemented yet.
    #[test]
    fn enabled_means_dispatchable() {
        for has_selection in [false, true] {
            let items = items(has_selection, false);
            let mut stack: Vec<&MenuItem> = items.iter().rev().collect();
            let mut leaves = 0;
            while let Some(item) = stack.pop() {
                if item.separator {
                    continue;
                }
                if item.children.is_empty() {
                    leaves += 1;
                    let live = command_for(item.id).is_some()
                        && (has_selection || !SELECTION_ROWS.contains(&item.id));
                    assert_eq!(item.enabled, live, "{}", item.label);
                } else {
                    assert!(item.enabled, "{} came back greyed", item.label);
                    stack.extend(item.children.iter().rev());
                }
            }
            assert!(leaves >= 60, "the table lost rows: {leaves} leaves");
            assert_eq!(items[2].enabled, has_selection, "剪切");
            assert_eq!(items[3].enabled, has_selection, "复制");
        }
        // The marker rows are dispatched now that the store is reimplemented, so they
        // are live rows.
        let markers = &items(false, false)[11];
        assert_eq!(markers.id, 1461);
        assert_eq!(markers.children.len(), 11, "自定义标记 lost rows");
        assert!(
            markers
                .children
                .iter()
                .filter(|c| !c.separator)
                .all(|c| c.enabled),
            "a marker row came back greyed"
        );
        let datetime = &items(false, false)[16];
        assert_eq!(datetime.id, 1417);
        assert!(datetime.children[0].enabled, "日期 came back greyed");
        // 自定义日期/时间 is dispatched now that the date/time dialog is reimplemented, so it is
        // a live row like its two siblings.
        assert!(
            datetime.children[2].enabled,
            "自定义日期/时间 came back greyed"
        );
        // 全屏显示 is dispatched now that the full-screen toggle is reimplemented,
        // so it is a live row.
        assert!(items(false, false)[18].enabled, "全屏显示 came back greyed");
        // 格式化缩进 is reimplemented, so its row is no longer greyed.
        let more = &items(true, false)[15];
        assert_eq!(more.id, 72);
        assert!(
            more.children.iter().any(|c| c.id == 51 && c.enabled),
            "格式化缩进 came back greyed"
        );
    }

    /// Every leaf id is either dispatched or listed as deferred, so a row can
    /// never be dropped from the menu by forgetting to wire it.
    #[test]
    fn every_leaf_row_is_accounted_for() {
        let leaves = leaf_ids(&items(true, false));
        assert!(!leaves.contains(&TEAR_OFF));
        for id in &leaves {
            assert!(
                command_for(*id).is_some() || DEFERRED.contains(id),
                "row {id} is neither dispatched nor deferred"
            );
        }
        let dispatched = leaves
            .iter()
            .filter(|id| command_for(**id).is_some())
            .count();
        // Every leaf this table defines but the tear-off row, which
        // is still unimplemented and stays greyed rather
        // than enabled-and-inert.
        assert!(
            dispatched >= 68,
            "the command surface fell back to {dispatched} rows"
        );
        // The two tables must not overlap: a dispatched row that is also listed
        // as deferred would be greyed out while still working.
        for id in &leaves {
            if command_for(*id).is_some() {
                assert!(
                    !DEFERRED.contains(id),
                    "row {id} is dispatched and deferred"
                );
            }
        }
    }

    #[test]
    fn popup_children_are_not_leaves_of_the_parent() {
        let items = items(true, false);
        let folding = &items[13];
        assert_eq!(folding.id, 1330);
        assert_eq!(folding.children[0].id, 1330);
        assert_eq!(leaf_ids(&folding.children).len(), 15);
    }
}
