//! Every command id the editor defines, and the matching `Command` variant.
//!
//! The table below has 343 records. The menu bar, the editor popup and
//! `key.ini` all name the same command, so one table answers for all three. It replaces the per-menu `id -> Command` arms: the
//! menu bar, the editor popup and `key.ini` all name the same command, so one
//! table can answer for all three and a row's id is no longer something to
//! re-derive per menu.
//!
//! The names `key.ini` uses are the same `cm_*` strings, so the id resolves to
//! a name and the name goes through the existing binding parser — one place
//! decides what a command *is*.

use crate::keys::Command;

/// `(id, name)` sorted by id; `cm_delete_duplicatedlines` and
/// `cm_tool_configtools`/`cm_config_tools` show the two shapes the table has
/// (a shared id, and a name this implementation's parser does not know).
const TABLE: &[(u32, &str)] = &[
    (1, "cm_move_left"),
    (2, "cm_move_right"),
    (3, "cm_move_up"),
    (4, "cm_move_down"),
    (5, "cm_move_toheadofline"),
    (6, "cm_move_toendofline"),
    (7, "cm_move_toheadoffile"),
    (8, "cm_move_toendoffile"),
    (9, "cm_move_prevpage"),
    (10, "cm_move_nextpage"),
    (11, "cm_move_nextword"),
    (12, "cm_move_prevword"),
    (13, "cm_select_toleft"),
    (14, "cm_select_toright"),
    (15, "cm_select_toup"),
    (16, "cm_select_todown"),
    (17, "cm_select_toheadofline"),
    (18, "cm_select_toendofline"),
    (19, "cm_select_toheadoffile"),
    (20, "cm_select_toendoffile"),
    (21, "cm_select_toprevpage"),
    (22, "cm_select_tonextpage"),
    (23, "cm_select_tonextword"),
    (24, "cm_select_toprevword"),
    (26, "cm_delete_prev"),
    (27, "cm_delete_next"),
    (28, "cm_delete_prevword"),
    (29, "cm_delete_nextword"),
    (30, "cm_delete_line"),
    (36, "cm_edit_ovrins"),
    (37, "cm_edit_rawcopy"),
    (38, "cm_move_scrollup"),
    (39, "cm_move_scrolldown"),
    (40, "cm_move_middlecaret"),
    (44, "cm_insert_newlineafter"),
    (45, "cm_insert_newlinebefore"),
    (46, "cm_select_word"),
    (47, "cm_edit_increaseindent"),
    (48, "cm_edit_decreaseindent"),
    (49, "cm_move_lineup"),
    (50, "cm_move_linedown"),
    (51, "cm_edit_formatindent"),
    (52, "cm_folding_expandcurlevel"),
    (54, "cm_edit_duplicateline"),
    (55, "cm_select_toprevline"),
    (56, "cm_select_tonextline"),
    (57, "cm_edit_toupper"),
    (58, "cm_edit_tolower"),
    (59, "cm_move_middleline"),
    (60, "cm_move_brace"),
    (61, "cm_edit_pasteindent"),
    (62, "cm_folding_foldcurlevel"),
    (63, "cm_delete_leadingspace"),
    (64, "cm_delete_tailingspace"),
    (66, "cm_insert_newline"),
    (67, "cm_select_topair"),
    (68, "cm_edit_copyline"),
    (69, "cm_edit_tab2space"),
    (70, "cm_edit_space2tab"),
    (72, "cm_edit_joinlines"),
    (73, "cm_move_toheadofsubline"),
    (74, "cm_move_toendofsubline"),
    (75, "cm_select_toheadofsubline"),
    (76, "cm_select_toendofsubline"),
    (77, "cm_select_topair2"),
    (79, "cm_delete_toheadofline"),
    (80, "cm_delete_toendofline"),
    (81, "cm_edit_transpose"),
    (82, "cm_edit_invertcase"),
    (83, "cm_edit_leadingtabs2space"),
    (84, "cm_edit_leadingspace2tab"),
    (85, "cm_delete_word"),
    (86, "cm_select_line"),
    (87, "cm_edit_cutappend"),
    (88, "cm_edit_copyappend"),
    (89, "cm_edit_copymarkedlines"),
    (90, "cm_edit_cutmarkedlines"),
    (91, "cm_edit_pastecolumn"),
    (92, "cm_edit_copyasrtf"),
    (94, "cm_edit_pastetail"),
    (95, "cm_move_nextpara"),
    (96, "cm_move_prevpara"),
    (97, "cm_edit_tofullwidth"),
    (98, "cm_edit_tohalfwidth"),
    (102, "cm_edit_capitalize"),
    (104, "cm_move_nexteditpos"),
    (105, "cm_move_preveditpos"),
    (106, "cm_select_splitintolines"),
    (108, "cm_edit_duplicateselection"),
    (109, "cm_select_restore"),
    (112, "cm_edit_pastewith0"),
    (113, "cm_move_scrolldownscreen"),
    (114, "cm_move_scrollupscreen"),
    (115, "cm_select_colup"),
    (116, "cm_select_coldown"),
    (117, "cm_select_colleft"),
    (118, "cm_select_colright"),
    (119, "cm_move_nextcaretpos"),
    (120, "cm_move_prevcaretpos"),
    (1200, "cm_window_next"),
    (1201, "cm_window_prev"),
    (1202, "cm_file_closeall"),
    (1203, "cm_file_closeothers"),
    (1204, "cm_window_vertsplit"),
    (1205, "cm_window_horzsplit"),
    (1206, "cm_file_close"),
    (1207, "cm_window_clone"),
    (1208, "cm_window_syncscroll"),
    (1209, "cm_window_tovertgroup"),
    (1210, "cm_window_mergegroup"),
    (1211, "cm_window_tohorzgroup"),
    (1212, "cm_window_next_taborder"),
    (1213, "cm_window_prev_taborder"),
    (1214, "cm_window_topmost"),
    (1215, "cm_window_trans80"),
    (1216, "cm_window_trans60"),
    (1217, "cm_window_trans40"),
    (1218, "cm_window_trans20"),
    (1219, "cm_window_trans10"),
    (1220, "cm_window_opaque"),
    (1221, "cm_window_last"),
    (1222, "cm_window_toggleallpanels"),
    (1223, "cm_window_switchfocus"),
    (1226, "cm_window_trans_customize"),
    (1233, "cm_hex_new"),
    (1234, "cm_file_saveall"),
    (1236, "cm_file_clearhis"),
    (1237, "cm_file_reload"),
    (1238, "cm_file_other_templates"),
    (1239, "cm_ftp_manager"),
    (1240, "cm_file_open_recently_closed"),
    (1241, "cm_file_save_newsession"),
    (1243, "cm_file_save_currentsession"),
    (1244, "cm_file_closeleft"),
    (1245, "cm_file_closeright"),
    (1248, "cm_file_open_folder"),
    (1271, "cm_highlight_color"),
    (1276, "cm_marker_toggle"),
    (1277, "cm_marker_deleteall"),
    (1278, "cm_marker_prev"),
    (1279, "cm_marker_next"),
    (1280, "cm_tooltip_next"),
    (1281, "cm_tooltip_prev"),
    (1282, "cm_tooltip_clearall"),
    (1283, "cm_wrap_pixelwidth"),
    (1285, "cm_dock_filemanager"),
    (1286, "cm_dock_favorite"),
    (1288, "cm_view_webpreview2"),
    (1289, "cm_dock_bookmarkmanager"),
    (1290, "cm_view_webpreview"),
    (1291, "cm_dock_openfiles"),
    (1292, "cm_dock_dirview"),
    (1293, "cm_wrap_fixedcolwithtab"),
    (1294, "cm_wrap_cancel"),
    (1295, "cm_wrap_fixedcol"),
    (1296, "cm_wrap_winedge"),
    (1297, "cm_wrap_smart"),
    (1298, "cm_config_theme"),
    (1300, "cm_view_linenumber"),
    (1301, "cm_view_ruler"),
    (1302, "cm_view_whitespace"),
    (1303, "cm_view_endofline"),
    (1304, "cm_view_endoffile"),
    (1305, "cm_view_activeline"),
    (1306, "cm_view_controlchars"),
    (1307, "cm_view_indentline"),
    (1308, "cm_wrap_indentsublines"),
    (1309, "cm_view_foldingtail"),
    (1310, "cm_view_showallinsel"),
    (1311, "cm_view_boundaryline"),
    (1312, "cm_dock_outline"),
    (1313, "cm_highlight_sel"),
    (1314, "cm_view_modebar"),
    (1315, "cm_view_next_outputresult"),
    (1316, "cm_view_prev_outputresult"),
    (1318, "cm_misc_usermarker"),
    (1321, "cm_config_setfont"),
    (1322, "cm_dock_command"),
    (1323, "cm_view_linespacing"),
    (1324, "cm_view_tabbar"),
    (1325, "cm_dock_output"),
    (1326, "cm_highlight_brace"),
    (1327, "cm_highlight_urlemail"),
    (1328, "cm_view_fullscreen"),
    (1329, "cm_dock_snippet"),
    (1330, "cm_folding_cancel"),
    (1331, "cm_folding_syntax"),
    (1332, "cm_folding_indent"),
    (1334, "cm_emoji_show"),
    (1336, "cm_config_options"),
    (1337, "cm_view_colmarker"),
    (1338, "cm_view_colmode"),
    (1339, "cm_folding_foldall"),
    (1340, "cm_folding_expandall"),
    (1401, "cm_find_next"),
    (1402, "cm_find_prev"),
    (1403, "cm_find_all"),
    (1406, "cm_find_nextword"),
    (1407, "cm_find_prevword"),
    (1408, "cm_find_infiles"),
    (1409, "cm_delete_duplicatedlines"),
    (1410, "cm_select_quick"),
    (1412, "cm_edit_hexmode"),
    (1413, "cm_edit_linecomment"),
    (1414, "cm_edit_lineuncomment"),
    (1415, "cm_edit_blockcomment"),
    (1416, "cm_edit_blockuncomment"),
    (1417, "cm_insert_date"),
    (1418, "cm_insert_time"),
    (1419, "cm_misc_configdatetime"),
    (1420, "cm_replace_batch"),
    (1422, "cm_select_addall"),
    (1423, "cm_edit_partially"),
    (1424, "cm_view_zoominfont"),
    (1425, "cm_view_zoomoutfont"),
    (1426, "cm_move_goto"),
    (1427, "cm_doc_autocomplete"),
    (1428, "cm_delete_blanklines"),
    (1429, "cm_edit_eraseclipboard"),
    (1430, "cm_view_defaultfont"),
    (1431, "cm_view_cus1font"),
    (1432, "cm_view_cus2font"),
    (1433, "cm_view_cus3font"),
    (1434, "cm_view_cus4font"),
    (1435, "cm_view_cus5font"),
    (1453, "cm_tool_encrypt"),
    (1454, "cm_tool_decrypt"),
    (1456, "cm_doc_showcomplete"),
    (1459, "cm_config_syntax"),
    (1460, "cm_config_shortcut"),
    (1461, "cm_misc_usermarker_1"),
    (1462, "cm_misc_usermarker_2"),
    (1463, "cm_misc_usermarker_3"),
    (1464, "cm_misc_usermarker_4"),
    (1465, "cm_misc_usermarker_5"),
    (1466, "cm_misc_usermarker_6"),
    (1467, "cm_misc_usermarker_7"),
    (1468, "cm_misc_usermarker_8"),
    (1469, "cm_view_original_fontsize"),
    (1470, "cm_find_mini"),
    (1471, "cm_misc_usermarker_enable"),
    (1480, "cm_edit_alignleft"),
    (1481, "cm_edit_alignright"),
    (1482, "cm_edit_aligncenter"),
    (1486, "cm_emoji_insert"),
    (1487, "cm_select_anchor"),
    (1564, "cm_event_enable"),
    (1565, "cm_event_reload"),
    (1567, "cm_addon_installfromlocal"),
    (1568, "cm_file_saveas_script"),
    (1569, "cm_addon_manager"),
    (1570, "cm_macro_record"),
    (1571, "cm_macro_playback"),
    (1572, "cm_macro_saveas"),
    (1573, "cm_macro_repeat"),
    (1574, "cm_macro_reload"),
    // The command table has no names for them — so the this
    // implementation invents these two to resolve them through the one
    // id->name->Command chain.
    (1576, "cm_macro_load"),
    (1577, "cm_macro_edit"),
    (1578, "cm_macro_pause"),
    (1580, "cm_config_tools"),
    (1581, "cm_config_assoc"),
    (1601, "cm_tool_tool1"),
    (1602, "cm_tool_tool2"),
    (1603, "cm_tool_tool3"),
    (1604, "cm_tool_tool4"),
    (1605, "cm_tool_tool5"),
    (1606, "cm_tool_tool6"),
    (1607, "cm_tool_tool7"),
    (1608, "cm_tool_tool8"),
    (1609, "cm_tool_tool9"),
    (1612, "cm_tool_sequence"),
    (1613, "cm_tool_colorpicker"),
    (1614, "cm_tool_sumsel"),
    (1615, "cm_tool_diff"),
    (1616, "cm_tool_diff_external"),
    (1617, "cm_base64_utf8_encode"),
    (1618, "cm_base64_utf8_decode"),
    (1619, "cm_base64_more_encode"),
    (1620, "cm_base64_more_decode"),
    (1622, "cm_folding_level2"),
    (1623, "cm_folding_level3"),
    (1624, "cm_folding_level4"),
    (1625, "cm_folding_level5"),
    (1626, "cm_folding_level6"),
    (1627, "cm_folding_level7"),
    (1628, "cm_folding_level8"),
    (1629, "cm_folding_level9"),
    (1647, "cm_doc_reloadmode"),
    (1648, "cm_doc_rename"),
    (1649, "cm_doc_copyfilename"),
    (1650, "cm_doc_copypath"),
    (1651, "cm_doc_monitorclipboard"),
    (1652, "cm_tool_sort"),
    (1653, "cm_doc_reloadsyntax"),
    (1654, "cm_doc_autoindent"),
    (1655, "cm_doc_openfilefolder"),
    (1657, "cm_doc_autopair"),
    (1658, "cm_file_reloadwithencoding"),
    (1659, "cm_doc_convertencoding"),
    (1660, "cm_doc_viewscope"),
    (1661, "cm_doc_browsermode"),
    (1662, "cm_doc_changetype"),
    (1663, "cm_move_jumpoutofpair"),
    (1664, "cm_doc_focusmode"),
    (1665, "cm_doc_donotallowedit"),
    (1666, "cm_doc_vertmode"),
    (1667, "cm_doc_statistics"),
    (1668, "cm_doc_insertlfonwrap"),
    (1669, "cm_doc_convertencoding_multiple"),
    (1671, "cm_edit_softtab"),
    (1672, "cm_view_iconarea"),
    (1673, "cm_view_extendactiveline"),
    (1677, "cm_wrap_restoreaftercolsel"),
    (1681, "cm_tool_sortasc"),
    (1682, "cm_tool_sortdec"),
    (1683, "cm_spell_modal"),
    (1684, "cm_spell_instant"),
    (1695, "cm_edit_pastefromhistory"),
    (1760, "cm_goto_anything"),
    (1762, "cm_goto_symbol"),
    (2003, "cm_misc_checkver"),
    (2004, "cm_misc_reportbug"),
    (2005, "cm_misc_keymap"),
    (2009, "cm_misc_contexthelp"),
    (2010, "cm_misc_apphelp"),
    (2012, "cm_new_instance"),
    (36001, "cm_large_export_sel"),
    (36002, "cm_large_to_normal"),
    (36003, "cm_large_replace_export"),
    (36004, "cm_large_tail"),
    (36007, "cm_plugin_rescanparsers"),
    (36010, "cm_file_backup_history"),
    (36011, "cm_parser_market"),
    (36012, "cm_config_package"),
    (36013, "cm_script_new"),
    (57600, "cm_file_new"),
    (57601, "cm_file_open"),
    (57603, "cm_file_save"),
    (57604, "cm_file_saveas"),
    (57607, "cm_file_print"),
    (57609, "cm_file_printpreview"),
    (57634, "cm_edit_copy"),
    (57635, "cm_edit_cut"),
    (57636, "cm_find"),
    (57637, "cm_edit_paste"),
    (57641, "cm_replace"),
    (57642, "cm_select_all"),
    (57643, "cm_edit_undo"),
    (57644, "cm_edit_redo"),
    (57664, "cm_misc_about"),
    (57665, "cm_file_exit"),
    (59392, "cm_view_toolbar"),
    (59393, "cm_view_statusbar"),
];

/// The command a menu id selects, or `None` when this implementation has no equivalent:
/// either the id is outside the implemented surface, or its `cm_*` name is one the
/// command parser does not recognise yet.
pub fn command_for_id(id: u32) -> Option<Command> {
    // Four of its rows — minimap 1287, line state 1675, level background 1674,
    // bookmark 1676 — have no `cm_*` name in the command table, so only the id table
    // can make them live.
    if let Some(bit) = dg_render::ViewFlags::bit_for_menu_id(id) {
        // Rows for options this implementation cannot draw yet stay greyed rather than
        // promising a setting that changes nothing.
        return dg_render::ViewFlags::is_rendered(bit).then_some(Command::ViewToggle(bit));
    }
    let i = TABLE.partition_point(|&(n, _)| n < id);
    if i < TABLE.len() && TABLE[i].0 == id {
        // A menu row must not claim to be live for a command that does
        // nothing.
        return Command::parse(TABLE[i].1).filter(|c| *c != Command::Ignore);
    }
    // The lookup above cannot reach them even though this implementation implements the
    // commands. Left out, they sit greyed in a menu whose entries work — the mirror image
    // of the rule that keeps an unimplemented row disabled. 删除重复行(从上往下) is the same
    // case: one routine removes the redundant lines of a matched pair, and only the id says
    // which copy is dropped — `cm_delete_duplicatedlines` keeps the first, this row keeps
    // the last. The implementation has both directions, so the id is the only thing that
    // can tell them apart.
    Some(match id {
        1449 => Command::EolPc,
        1450 => Command::EolUnix,
        1451 => Command::EolMac,
        1452 => Command::DeleteDupLinesTop,
        // Only the id can reach it.
        1317 => Command::HideMenu,
        // 清除会话菜单 (1242): the frame's WM_COMMAND switch compares the id
        // directly (the dispatch compares the id against 1242, the session-clear
        // row) and
        // the command table has no row for it — like 1317, only the id can
        // make the menu row live.
        1242 => Command::FileClearSessionMenu,
        // 深色主题 (1711): the command table has no name for it, so the menu row
        // — whose check mark is asked through this function — is reached by id.
        1711 => Command::ViewDarkTheme,
        _ => return None,
    })
}

/// 配置模板 (1230): opens 选项 on its 模板 page. The command table has no
/// `cm_*` name for it, so the frame dispatches it by id; without this
/// carve-out the menu row would grey.
pub const CONFIG_TEMPLATE_ID: u32 = 1230;

/// 备份与历史 — the backup history dialog (backup versions, restore, save as).
/// The table names it (`cm_file_backup_history`) but the `Command` enum has no
/// arm for it, so it is another bare id the frame dispatches by number.
pub const BACKUP_HISTORY_ID: u32 = 36010;

/// The bare rows that dispatch by id alone: 使用虚拟空格 (1483) and the
/// 快速选择 pair (1484/1485) have no `cm_*` name anywhere, the
/// 监视该文件变更 rows (1751-1755) hold the per-document watch mode, and the
/// tab menu's 标签排序 pair (1272/1273), 分离到新实例 (1224) and 设置 (1335)
/// run through the frame's id dispatch the same way. They all run, so the bar
/// must not grey them.
pub const BARE_DISPATCH_IDS: &[u32] = &[
    1483, 1484, 1485, 1751, 1752, 1753, 1754, 1755, 1224, 1272, 1273, 1335,
];

/// Whether this build can answer an id at all.
///
/// Every surface that shows a command — the menu bar, the editor popup, the two
/// toolbars, the layout editor's list — asks this instead of restating the rule, so
/// a row that would do nothing can never look clickable in one place and greyed in
/// another. A button that opens a menu rather than dispatching a command is live by
/// that menu, which is why the toolbar adds its own carve-out.
///
/// 配置模板, 备份与历史 and the bare-dispatch list are the carve-outs: ids
/// the frame commands by number, not through the table.
pub fn is_answerable(id: u32) -> bool {
    id == CONFIG_TEMPLATE_ID
        || id == BACKUP_HISTORY_ID
        || BARE_DISPATCH_IDS.contains(&id)
        || command_for_id(id).is_some()
}

/// The `cm_*` name for an id, for diagnostics and the command-coverage count.
///
/// 外部工具 resolves through the table: 1580 is `cm_config_tools`
/// (the manager dialog) and 1601-1609 are `cm_tool_tool1..9`
/// (run the Nth tool — the tool-run dispatch turns the id into an index by
/// subtracting the first tool row's id).
pub fn name_for_id(id: u32) -> Option<&'static str> {
    let i = TABLE.partition_point(|&(n, _)| n < id);
    if i < TABLE.len() && TABLE[i].0 == id {
        Some(TABLE[i].1)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_sorted() {
        assert!(
            TABLE.windows(2).all(|w| w[0].0 < w[1].0),
            "TABLE not sorted/unique"
        );
    }

    /// 「重新扫描解析器」(cm_plugin_rescanparsers) 的 id 锁在 36007：它
    /// 一度排在 36005，和扩展菜单当时的两条内置脚本行撞车，点下去跑的
    /// 是脚本。那两行已撤，36005/36006 退役，不再分给新命令。
    #[test]
    fn rescan_parsers_sits_above_the_retired_script_ids() {
        assert_eq!(command_for_id(36005), None);
        assert_eq!(command_for_id(36006), None);
        assert_eq!(command_for_id(36007), Some(Command::RescanParsers));
    }

    /// 设置字体 (1321) 是命令表里唯一一个由对话框自己分发的 `cm_config_*`
    /// 行：名字在表里，但 [`Command`] 变体一度没有，于是 `command_for_id`
    /// 返回 None，菜单栏照着 `is_answerable` 把这一行画成灰的。
    #[test]
    fn set_font_row_is_answerable() {
        assert_eq!(name_for_id(1321), Some("cm_config_setfont"));
        assert_eq!(command_for_id(1321), Some(Command::ConfigFont));
        assert!(is_answerable(1321), "工具 → 设置 → 设置字体 不能是灰的");
    }

    /// 不在命令表里、直接按 id 分发的行同样必须可点：使用虚拟空格
    /// (1483)、快速选择的两只勾 (1484/1485) 和监视该文件变更的五行
    /// (1751-1755)。漏一个，菜单里就是一行灰的。
    #[test]
    fn bare_dispatch_rows_are_answerable() {
        for id in BARE_DISPATCH_IDS {
            assert!(is_answerable(*id), "id {id} 必须可点");
        }
        // and a row that really has no handler still reports false
        assert!(!is_answerable(0xFFFF));
    }

    /// 窗口菜单的分割/分组行都挂在命令表上：少了哪一行，那一行在
    /// 菜单里就是灰的。
    #[test]
    fn split_group_rows_are_answerable() {
        for id in [1204u32, 1205, 1207, 1208, 1209, 1210, 1211] {
            assert!(is_answerable(id), "id {id} 必须可点");
        }
        // id 与命令名必须指到同一个命令：两边各查一次，对不上就说明
        // 有一边漏配。
        for (id, name) in [
            (1204u32, "cm_window_vertsplit"),
            (1205, "cm_window_horzsplit"),
            (1207, "cm_window_clone"),
            (1208, "cm_window_syncscroll"),
            (1209, "cm_window_tovertgroup"),
            (1210, "cm_window_mergegroup"),
            (1211, "cm_window_tohorzgroup"),
        ] {
            assert_eq!(command_for_id(id), Command::parse(name), "id {id}");
        }
    }

    #[test]
    fn known_command_ids_resolve() {
        // Spot checks against the rows the menus actually carry, which is what
        // this table was validated against before it replaced them.
        assert_eq!(command_for_id(57600), Some(Command::New));
        assert_eq!(command_for_id(57634), Some(Command::Copy));
        assert_eq!(command_for_id(57635), Some(Command::Cut));
        assert_eq!(command_for_id(1695), Some(Command::PasteHistory));
        assert_eq!(command_for_id(1338), Some(Command::ColumnMode));
        // 编辑 → HEX模式 (cm_edit_hexmode): the view toggle the toolbar
        // button 95 shares the row id with.
        assert_eq!(command_for_id(1412), Some(Command::HexMode));
        // 文档 → 切换文件类型 (cm_doc_changetype): the 切换文件类型 dialog's list.
        assert_eq!(command_for_id(1662), Some(Command::ChangeType));
        // 编辑 → 使用软空格 (cm_edit_softtab)：走 id→位表的开关行，输入路径
        // （Tab/退格/缩进）与状态条都在，行必须是活的。
        assert_eq!(
            command_for_id(1671),
            Some(Command::ViewToggle(dg_render::ViewFlags::SOFT_TAB))
        );
        // 查看 → 深色主题 (1711): no name in the table; the frame answers the row
        // by id, and so does its check mark.
        assert_eq!(command_for_id(1711), Some(Command::ViewDarkTheme));
        // 查看 → 设置行高 (cm_view_linespacing)：倍数提示框，落到当前文档
        // 的额外行距上。
        assert_eq!(command_for_id(1323), Some(Command::ViewLineSpacing));
        // 编辑 → 批量替换 (cm_replace_batch)：成对的查找/替换列表框，整单
        // 或逐个跑在当前/全部打开的文档上。
        assert_eq!(command_for_id(1420), Some(Command::ReplaceBatch));
        // 工具 → 文件比较(外部) (cm_tool_diff_external)：两份打开的文档交给
        // 配置好的外部比较工具。
        assert_eq!(command_for_id(1616), Some(Command::ToolDiffExternal));
        // 新建 → 二进制 / 其它文件类型，以及 文件 → 新建实例。
        assert_eq!(command_for_id(1233), Some(Command::NewHex));
        assert_eq!(command_for_id(1238), Some(Command::NewFromType));
        assert_eq!(command_for_id(2012), Some(Command::NewInstance));
        assert!(is_answerable(2012), "新建实例 这一行不能是灰的");
        // 新建 → 配置模板 has no `cm_*` name; the frame answers it by id, so
        // the row still counts as live.
        assert!(is_answerable(CONFIG_TEMPLATE_ID));
        assert_eq!(command_for_id(0xFFFF), None);
    }

    /// The 外部工具 family: the manager row and the nine tool slots the menu's
    /// dynamic rows are commanded with.
    #[test]
    fn external_tool_ids_resolve() {
        assert_eq!(command_for_id(1580), Some(Command::ConfigTools));
        for i in 0..9u32 {
            assert_eq!(
                command_for_id(1601 + i),
                Some(Command::RunTool(i as usize)),
                "cm_tool_tool{} did not resolve",
                i + 1
            );
        }
        // The nine slots are the whole family; nothing past them.
        assert_eq!(command_for_id(1610), None);
    }

    /// The two halves of one rule: a row the menu bar enables must have a
    /// handler, and a command that has none yet must stay greyed.
    #[test]
    fn navigation_rows_are_all_live() {
        for id in [
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
            40, 49, 50, 55, 56, 59, 60, 67, 73, 74, 75, 76, 77, 86, 95, 96, 104, 105, 109, 119,
            120, 1449, 1450, 1451, 1663,
        ] {
            assert!(command_for_id(id).is_some(), "id {id} greyed");
        }
        // 编辑位置对（104/105）走编辑位置环：apply_edit 落笔落账，命令
        // 跳转，收尾与光标环相同。
        for (id, name) in [(104u32, "cm_move_nexteditpos"), (105, "cm_move_preveditpos")] {
            assert_eq!(command_for_id(id), Command::parse(name), "id {id}");
        }
    }
}
