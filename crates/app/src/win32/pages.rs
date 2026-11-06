//! The 选项 dialog control tables dialog layouts: dialog-unit geometry,
//! window styles and control ids. A page is built here rather than loaded
//! as a resource, so the dialog manager lays the controls out exactly as
//! a template would while the captions stay in `optionsdlg`'s 2052
//! tables.

use crate::win32::optionsdlg::{
    build_template, Class, Item, ATOM_BUTTON, ATOM_COMBOBOX, ATOM_EDIT,
    ATOM_LISTBOX, ATOM_STATIC,
};

/// An `Item` in one line, so a page reads as a layout rather than as a
/// struct literal per control.
const fn item(
    id: i32,
    class: Class,
    style: u32,
    x: i16,
    y: i16,
    w: i16,
    h: i16,
    label: &'static str,
) -> Item {
    Item {
        id,
        class,
        style,
        x,
        y,
        w,
        h,
        label,
    }
}

/// A page template: dialog style, size in dialog units, and the controls.
#[derive(Clone, Copy)]
pub(super) struct PageDef {
    pub(super) style: u32,
    pub(super) w: u16,
    pub(super) h: u16,
    pub(super) items: &'static [Item],
}

/// Page 213's control list.
const PAGE_213: &[Item] = &[
    item(1211, Class::Atom(ATOM_BUTTON), 0x5001_0003, 0, 17, 240, 10, ""),
    item(1201, Class::Name("SysListView32"), 0x5081_c00d, 0, 30, 135, 84, ""),
    item(1202, Class::Atom(ATOM_BUTTON), 0x5001_0000, 137, 30, 16, 14, "+"),
    item(1203, Class::Atom(ATOM_BUTTON), 0x5001_0000, 137, 45, 16, 14, "-"),
    item(1204, Class::Atom(ATOM_BUTTON), 0x5001_0000, 137, 61, 16, 14, ""),
    item(1205, Class::Atom(ATOM_BUTTON), 0x5001_0000, 137, 77, 16, 14, ""),
    item(1206, Class::Atom(ATOM_EDIT), 0x5081_0080, 60, 119, 75, 13, ""),
    item(1200, Class::Atom(ATOM_COMBOBOX), 0x5021_0203, 60, 136, 75, 30, ""),
    item(1208, Class::Atom(ATOM_EDIT), 0x5081_0080, 60, 153, 75, 13, ""),
    item(1207, Class::Atom(ATOM_BUTTON), 0x5001_0000, 139, 153, 16, 13, "..."),
    item(1209, Class::Atom(ATOM_BUTTON), 0x5001_0001, 157, 153, 16, 13, ""),
    item(3000, Class::Atom(ATOM_STATIC), 0x5002_0000, 0, 121, 55, 9, ""),
    item(3001, Class::Atom(ATOM_STATIC), 0x5002_0000, 0, 137, 50, 10, ""),
    item(3002, Class::Atom(ATOM_STATIC), 0x5002_0000, 0, 155, 55, 10, ""),
    item(1210, Class::Atom(ATOM_BUTTON), 0x5001_0003, 0, 171, 250, 12, ""),
];

/// Page 215's control list.
const PAGE_215: &[Item] = &[
    item(1200, Class::Atom(ATOM_LISTBOX), 0x50a1_0103, 0, 30, 115, 160, ""),
    item(3000, Class::Atom(ATOM_STATIC), 0x5002_0000, 0, 17, 190, 12, ""),
    item(3001, Class::Atom(ATOM_STATIC), 0x5002_0000, 122, 32, 110, 10, ""),
    item(3002, Class::Atom(ATOM_STATIC), 0x5002_0000, 122, 64, 110, 11, ""),
    item(1202, Class::Atom(ATOM_STATIC), 0x5002_0000, 122, 46, 110, 12, ""),
    item(1203, Class::Atom(ATOM_STATIC), 0x5002_0000, 122, 77, 110, 80, ""),
];

/// Page 217's control list.
const PAGE_217: &[Item] = &[
    item(2000, Class::Name("SysTreeView32"), 0x5081_0800, 0, 18, 255, 190, ""),
    item(3000, Class::Atom(ATOM_STATIC), 0x4002_0000, 105, 45, 104, 10, ""),
    item(3001, Class::Atom(ATOM_STATIC), 0x4002_0000, 0, 24, 105, 10, ""),
    item(3002, Class::Atom(ATOM_STATIC), 0x4002_0000, 120, 35, 104, 10, ""),
    item(1202, Class::Atom(ATOM_STATIC), 0x4002_0000, 0, 37, 88, 10, ""),
    item(1203, Class::Atom(ATOM_STATIC), 0x4002_0000, 0, 52, 195, 10, ""),
    item(1200, Class::Atom(ATOM_STATIC), 0x4002_0000, 0, 66, 200, 10, ""),
    item(1204, Class::Atom(ATOM_STATIC), 0x4002_0000, 0, 81, 135, 10, ""),
    item(1216, Class::Atom(ATOM_STATIC), 0x4002_0000, 140, 79, 40, 14, ""),
    item(1, Class::Atom(ATOM_STATIC), 0x4002_0000, 182, 82, 37, 11, "tab width"),
    item(1205, Class::Atom(ATOM_STATIC), 0x4002_0000, 0, 96, 210, 10, ""),
    item(1207, Class::Atom(ATOM_STATIC), 0x4002_0000, 0, 110, 125, 10, ""),
    item(1208, Class::Atom(ATOM_STATIC), 0x4002_0000, 127, 109, 105, 11, ""),
    item(1209, Class::Atom(ATOM_STATIC), 0x4002_0000, 0, 124, 195, 10, ""),
    item(1210, Class::Atom(ATOM_STATIC), 0x4002_0000, 0, 138, 205, 11, ""),
    item(1211, Class::Atom(ATOM_STATIC), 0x4002_0000, 0, 150, 205, 13, ""),
    item(1212, Class::Atom(ATOM_STATIC), 0x4002_0000, 0, 164, 230, 14, ""),
    item(1214, Class::Atom(ATOM_STATIC), 0x4002_0000, 0, 180, 220, 10, ""),
    item(1215, Class::Atom(ATOM_STATIC), 0x4002_0000, 25, 180, 205, 10, ""),
    item(1217, Class::Atom(ATOM_STATIC), 0x4002_0000, 25, 180, 205, 10, ""),
];

/// Page 224's control list.
const PAGE_224: &[Item] = &[
    item(1216, Class::Atom(ATOM_BUTTON), 0x5001_0000, 5, 20, 86, 14, ""),
    item(3000, Class::Atom(ATOM_BUTTON), 0x5000_0007, 0, 41, 245, 43, ""),
    item(3001, Class::Atom(ATOM_STATIC), 0x5002_0000, 92, 54, 60, 11, ""),
    item(3002, Class::Atom(ATOM_STATIC), 0x5002_0000, 5, 54, 80, 11, ""),
    item(3006, Class::Atom(ATOM_STATIC), 0x5002_0000, 164, 54, 70, 10, ""),
    item(1201, Class::Atom(ATOM_COMBOBOX), 0x5021_0203, 5, 65, 80, 30, ""),
    item(1200, Class::Atom(ATOM_COMBOBOX), 0x5001_0203, 92, 65, 65, 30, ""),
    item(1217, Class::Atom(ATOM_COMBOBOX), 0x5021_0203, 164, 65, 65, 30, ""),
    item(1212, Class::Atom(ATOM_BUTTON), 0x5001_0003, 5, 88, 225, 12, ""),
    item(
        1220,
        Class::Atom(ATOM_BUTTON),
        0x5001_0003,
        5,
        101,
        235,
        13,
        "Set encoding to UTF-8 without BOM on ANSI files",
    ),
    item(1221, Class::Atom(ATOM_BUTTON), 0x5001_0003, 5, 116, 245, 11, ""),
    item(3003, Class::Atom(ATOM_BUTTON), 0x5000_0007, 0, 131, 245, 74, ""),
    item(3004, Class::Atom(ATOM_STATIC), 0x5002_0000, 5, 144, 125, 13, ""),
    item(1215, Class::Atom(ATOM_COMBOBOX), 0x5021_0203, 5, 156, 95, 30, ""),
    item(1207, Class::Atom(ATOM_BUTTON), 0x5001_0003, 136, 142, 105, 11, ""),
    item(1213, Class::Atom(ATOM_COMBOBOX), 0x5021_0203, 135, 156, 89, 30, ""),
    item(3005, Class::Atom(ATOM_STATIC), 0x5002_0000, 5, 174, 105, 10, ""),
    item(1218, Class::Atom(ATOM_COMBOBOX), 0x5001_0203, 5, 186, 95, 30, ""),
];

/// Page 238's control list.
const PAGE_238: &[Item] = &[
    item(2000, Class::Name("SysTreeView32"), 0x5081_2800, 0, 18, 245, 190, ""),
    item(1202, Class::Atom(ATOM_STATIC), 0x4002_0000, 5, 36, 235, 12, ""),
    item(1203, Class::Atom(ATOM_STATIC), 0x4002_0000, 5, 51, 235, 12, ""),
    item(1209, Class::Atom(ATOM_STATIC), 0x4002_0000, 5, 66, 230, 12, ""),
    item(1211, Class::Atom(ATOM_STATIC), 0x4002_0000, 6, 138, 225, 12, ""),
    item(1210, Class::Atom(ATOM_STATIC), 0x4002_0000, 5, 102, 230, 11, ""),
    item(1215, Class::Atom(ATOM_STATIC), 0x4002_0000, 5, 155, 175, 40, ""),
    item(1216, Class::Atom(ATOM_STATIC), 0x4002_0000, 0, 33, 95, 15, ""),
    item(1217, Class::Atom(ATOM_STATIC), 0x4002_0000, 0, 68, 100, 11, ""),
    item(1218, Class::Atom(ATOM_STATIC), 0x4002_0000, 5, 54, 135, 10, ""),
    item(1219, Class::Atom(ATOM_STATIC), 0x4002_0000, 0, 68, 100, 11, ""),
    item(1220, Class::Atom(ATOM_STATIC), 0x4002_0000, 0, 68, 100, 11, ""),
];

/// Page 244's control list.
const PAGE_244: &[Item] = &[
    item(1200, Class::Atom(ATOM_EDIT), 0x5081_0080, 5, 35, 200, 14, ""),
    item(3000, Class::Atom(ATOM_STATIC), 0x5002_0000, 5, 20, 215, 11, ""),
];

/// Page 246's control list.
const PAGE_246: &[Item] = &[
    item(1201, Class::Atom(ATOM_BUTTON), 0x5001_0003, 0, 25, 200, 15, ""),
    item(3002, Class::Atom(ATOM_BUTTON), 0x5000_0007, 0, 46, 240, 55, ""),
    item(1200, Class::Atom(ATOM_BUTTON), 0x5001_0003, 6, 56, 215, 15, ""),
    item(1203, Class::Atom(ATOM_BUTTON), 0x5001_0003, 6, 71, 210, 15, ""),
    item(1202, Class::Atom(ATOM_BUTTON), 0x5001_0003, 6, 84, 195, 15, ""),
    item(3000, Class::Atom(ATOM_STATIC), 0x5002_0000, 0, 108, 240, 10, ""),
    item(1204, Class::Atom(ATOM_EDIT), 0x5081_0080, 0, 121, 185, 14, ""),
    item(1205, Class::Atom(ATOM_BUTTON), 0x5001_0000, 187, 121, 16, 14, "..."),
    item(3001, Class::Atom(ATOM_STATIC), 0x5002_0000, 0, 139, 235, 10, ""),
];

/// Page 247's control list.
const PAGE_247: &[Item] = &[
    item(1204, Class::Atom(ATOM_BUTTON), 0x5001_0003, 0, 16, 205, 13, ""),
    item(3000, Class::Atom(ATOM_BUTTON), 0x5000_0007, 0, 31, 160, 70, ""),
    item(3001, Class::Atom(ATOM_STATIC), 0x5002_0000, 60, 71, 55, 15, ""),
    item(3002, Class::Atom(ATOM_STATIC), 0x5002_0000, 18, 56, 45, 15, ""),
    item(3003, Class::Atom(ATOM_STATIC), 0x5002_0000, 60, 41, 50, 14, ""),
    item(3004, Class::Atom(ATOM_STATIC), 0x5002_0000, 102, 56, 50, 14, ""),
    item(1200, Class::Atom(ATOM_EDIT), 0x5081_2000, 60, 51, 40, 14, ""),
    item(1201, Class::Atom(ATOM_EDIT), 0x5081_2000, 18, 66, 40, 14, ""),
    item(1202, Class::Atom(ATOM_EDIT), 0x5081_2000, 60, 81, 40, 14, ""),
    item(1203, Class::Atom(ATOM_EDIT), 0x5081_2000, 102, 66, 40, 14, ""),
    item(3005, Class::Atom(ATOM_STATIC), 0x5002_0000, 0, 106, 90, 10, ""),
    item(2405, Class::Atom(ATOM_EDIT), 0x5081_0004, 0, 117, 220, 35, ""),
    item(1207, Class::Atom(ATOM_BUTTON), 0x5001_0000, 222, 116, 14, 13, ""),
    item(3006, Class::Atom(ATOM_STATIC), 0x5002_0000, 0, 156, 90, 10, ""),
    item(2406, Class::Atom(ATOM_EDIT), 0x5081_0004, 0, 168, 220, 35, ""),
    item(1208, Class::Atom(ATOM_BUTTON), 0x5001_0000, 222, 167, 14, 13, ""),
];

/// Page 252's control list.
const PAGE_252: &[Item] = &[
    item(2000, Class::Name("SysTreeView32"), 0x5081_2800, 0, 18, 245, 195, ""),
    item(1200, Class::Atom(ATOM_STATIC), 0x4002_0000, 10, 195, 245, 12, ""),
    item(1201, Class::Atom(ATOM_STATIC), 0x4002_0000, 0, 47, 250, 12, ""),
    item(1202, Class::Atom(ATOM_STATIC), 0x4002_0000, 0, 76, 250, 12, ""),
    item(1204, Class::Atom(ATOM_STATIC), 0x4002_0000, 0, 138, 245, 10, ""),
    item(1205, Class::Atom(ATOM_STATIC), 0x4002_0000, 0, 170, 240, 11, ""),
];

/// Page 254's control list.
const PAGE_254: &[Item] = &[
    item(3000, Class::Atom(ATOM_STATIC), 0x5002_0000, 120, 115, 125, 11, ""),
    item(1200, Class::Atom(ATOM_COMBOBOX), 0x5001_0203, 120, 127, 85, 30, ""),
    item(3003, Class::Atom(ATOM_STATIC), 0x5002_0000, 0, 18, 85, 10, ""),
    item(1203, Class::Atom(ATOM_LISTBOX), 0x50a1_0141, 0, 30, 85, 80, ""),
    item(1206, Class::Atom(ATOM_BUTTON), 0x5001_0000, 87, 56, 20, 14, ""),
    item(1207, Class::Atom(ATOM_BUTTON), 0x5001_0000, 87, 72, 20, 14, ""),
    item(3004, Class::Atom(ATOM_STATIC), 0x5002_0000, 110, 20, 105, 10, ""),
    item(1208, Class::Atom(ATOM_LISTBOX), 0x50a1_0141, 110, 30, 85, 80, ""),
    item(1204, Class::Atom(ATOM_BUTTON), 0x5001_0000, 198, 56, 20, 14, ""),
    item(1205, Class::Atom(ATOM_BUTTON), 0x5001_0000, 198, 72, 20, 14, ""),
    item(3005, Class::Atom(ATOM_STATIC), 0x5002_0000, 0, 115, 110, 11, ""),
    item(1202, Class::Atom(ATOM_COMBOBOX), 0x5001_0203, 0, 127, 85, 13, ""),
    item(1209, Class::Atom(ATOM_BUTTON), 0x5001_0003, 0, 144, 245, 11, ""),
];

/// Page 272's control list.
const PAGE_272: &[Item] = &[
    item(3000, Class::Atom(ATOM_STATIC), 0x5002_0000, 0, 16, 200, 10, ""),
    item(1203, Class::Name("SysListView32"), 0x5081_000d, 0, 29, 240, 70, ""),
    item(1200, Class::Atom(ATOM_BUTTON), 0x5001_0001, 0, 101, 50, 14, ""),
    item(1201, Class::Atom(ATOM_BUTTON), 0x5001_0000, 52, 101, 50, 14, ""),
    item(1202, Class::Atom(ATOM_BUTTON), 0x5001_0000, 105, 101, 50, 14, ""),
    item(3001, Class::Atom(ATOM_STATIC), 0x5002_0000, 0, 121, 205, 10, ""),
    item(1204, Class::Atom(ATOM_COMBOBOX), 0x5001_0203, 0, 133, 155, 30, ""),
    item(3002, Class::Atom(ATOM_STATIC), 0x5002_0000, 0, 151, 205, 10, ""),
    item(1205, Class::Atom(ATOM_EDIT), 0x5081_0080, 0, 163, 155, 13, ""),
    item(1206, Class::Atom(ATOM_BUTTON), 0x5001_0000, 157, 163, 13, 13, "..."),
];

/// Page 275's control list.
const PAGE_275: &[Item] = &[
    item(2000, Class::Name("SysTreeView32"), 0x5081_2800, 0, 18, 245, 190, ""),
    item(1200, Class::Atom(ATOM_STATIC), 0x4002_0000, 0, 5, 95, 10, ""),
    item(1201, Class::Atom(ATOM_STATIC), 0x4002_0000, 115, 5, 110, 10, ""),
    item(1202, Class::Atom(ATOM_STATIC), 0x4002_0000, 0, 66, 110, 10, ""),
    item(1203, Class::Atom(ATOM_STATIC), 0x4002_0000, 0, 35, 110, 10, ""),
    item(1204, Class::Atom(ATOM_STATIC), 0x4002_0000, 115, 35, 115, 10, ""),
    item(1205, Class::Atom(ATOM_STATIC), 0x4002_0000, 116, 66, 115, 11, ""),
    item(1208, Class::Atom(ATOM_STATIC), 0x4002_0000, 0, 96, 115, 11, ""),
    item(1210, Class::Atom(ATOM_STATIC), 0x4002_0000, 116, 96, 121, 9, ""),
    item(1211, Class::Atom(ATOM_STATIC), 0x4002_0000, 0, 124, 117, 10, ""),
    item(1206, Class::Atom(ATOM_STATIC), 0x4002_0000, 5, 165, 205, 10, ""),
    item(1207, Class::Atom(ATOM_STATIC), 0x4002_0000, 5, 179, 210, 10, ""),
    item(1209, Class::Atom(ATOM_STATIC), 0x4002_0000, 5, 192, 210, 11, ""),
];

/// Page 285's control list.
const PAGE_285: &[Item] = &[
    item(1200, Class::Atom(ATOM_BUTTON), 0x5001_0003, 0, 34, 205, 11, ""),
    item(3003, Class::Atom(ATOM_STATIC), 0x5002_0000, 0, 52, 187, 12, ""),
    item(1207, Class::Atom(ATOM_EDIT), 0x5081_0080, 203, 50, 47, 14, ""),
    item(1208, Class::Name("msctls_updown32"), 0x5000_00b6, 237, 50, 11, 14, ""),
    item(3005, Class::Atom(ATOM_STATIC), 0x5002_0000, 0, 70, 185, 14, ""),
    item(1209, Class::Atom(ATOM_EDIT), 0x5081_0080, 203, 67, 47, 14, ""),
    item(1210, Class::Name("msctls_updown32"), 0x5000_00b6, 237, 67, 11, 14, ""),
    item(3006, Class::Atom(ATOM_STATIC), 0x5002_0000, 0, 88, 185, 12, ""),
    item(1213, Class::Atom(ATOM_EDIT), 0x5081_0080, 203, 86, 47, 14, ""),
    item(1214, Class::Name("msctls_updown32"), 0x5000_00b6, 237, 86, 11, 14, ""),
    item(3007, Class::Atom(ATOM_STATIC), 0x5002_0000, 0, 106, 185, 12, ""),
    item(1215, Class::Atom(ATOM_EDIT), 0x5081_0080, 203, 104, 47, 14, ""),
    item(1216, Class::Name("msctls_updown32"), 0x5000_00b6, 237, 104, 11, 14, ""),
    item(3008, Class::Atom(ATOM_STATIC), 0x5002_0000, 0, 124, 185, 12, ""),
    item(1217, Class::Atom(ATOM_EDIT), 0x5081_0080, 203, 122, 47, 14, ""),
    item(1218, Class::Name("msctls_updown32"), 0x5000_00b6, 237, 122, 11, 14, ""),
    item(1211, Class::Atom(ATOM_BUTTON), 0x5001_0003, 0, 142, 240, 13, ""),
    item(1212, Class::Atom(ATOM_EDIT), 0x5081_0080, 0, 158, 185, 14, ""),
    item(1206, Class::Atom(ATOM_BUTTON), 0x5001_0000, 190, 158, 16, 14, "..."),
];

/// Page 286's control list.
const PAGE_286: &[Item] = &[
    item(3000, Class::Atom(ATOM_STATIC), 0x5002_0000, 7, 34, 160, 10, ""),
    item(3001, Class::Atom(ATOM_STATIC), 0x5002_0000, 7, 93, 60, 11, ""),
    item(3002, Class::Atom(ATOM_STATIC), 0x5002_0000, 7, 112, 60, 11, ""),
    item(3003, Class::Atom(ATOM_STATIC), 0x5002_0000, 7, 129, 60, 12, ""),
    item(3004, Class::Atom(ATOM_STATIC), 0x5002_0000, 7, 143, 65, 11, ""),
    item(1200, Class::Atom(ATOM_BUTTON), 0x5002_0009, 7, 47, 135, 10, ""),
    item(1201, Class::Atom(ATOM_BUTTON), 0x5000_0009, 7, 61, 175, 11, ""),
    item(1202, Class::Atom(ATOM_BUTTON), 0x5000_0009, 7, 76, 125, 11, ""),
    item(1203, Class::Atom(ATOM_EDIT), 0x5081_0080, 77, 91, 155, 14, ""),
    item(1204, Class::Atom(ATOM_EDIT), 0x5081_0080, 77, 108, 40, 14, ""),
    item(1205, Class::Atom(ATOM_EDIT), 0x5081_0080, 77, 125, 155, 14, ""),
    item(1206, Class::Atom(ATOM_EDIT), 0x5081_00a0, 77, 141, 155, 14, ""),
];

/// Page 296's control list.
const PAGE_296: &[Item] = &[
    item(3000, Class::Atom(ATOM_STATIC), 0x5002_0000, 5, 17, 185, 11, ""),
    item(1200, Class::Name("SysListView32"), 0x5081_680d, 3, 30, 140, 175, ""),
    item(1201, Class::Atom(ATOM_BUTTON), 0x5001_0000, 148, 30, 25, 14, ""),
    item(1202, Class::Atom(ATOM_BUTTON), 0x5001_0001, 148, 45, 25, 14, ""),
];

/// Page 207's control list. The two lists sit above a theme row: the icon
/// theme picker (label, dropdown, and the button that opens the theme
/// folders).
const PAGE_207: &[Item] = &[
    item(2000, Class::Atom(ATOM_STATIC), 0x5002_0000, 0, 5, 240, 10, ""),
    item(1201, Class::Atom(ATOM_LISTBOX), 0x50a1_0161, 0, 17, 105, 168, ""),
    item(1202, Class::Atom(ATOM_LISTBOX), 0x50a1_0161, 135, 17, 105, 168, ""),
    item(1203, Class::Atom(ATOM_BUTTON), 0x5001_0000, 110, 45, 20, 14, ""),
    item(1204, Class::Atom(ATOM_BUTTON), 0x5001_0001, 110, 60, 20, 14, ""),
    item(1205, Class::Atom(ATOM_BUTTON), 0x5001_0000, 110, 75, 20, 14, ""),
    item(1206, Class::Atom(ATOM_BUTTON), 0x5001_0000, 110, 112, 20, 14, ""),
    item(1207, Class::Atom(ATOM_BUTTON), 0x5001_0000, 110, 127, 20, 14, ""),
    item(3000, Class::Atom(ATOM_STATIC), 0x5002_0000, 0, 191, 70, 10, ""),
    item(1208, Class::Atom(ATOM_COMBOBOX), 0x5021_0203, 70, 188, 116, 120, ""),
    item(1209, Class::Atom(ATOM_BUTTON), 0x5001_0000, 190, 188, 50, 14, ""),
];

/// The 主机/目录 mapping editor opened from the 网络 page (page 273).
const HOST_ITEMS: &[Item] = &[
    item(1, Class::Atom(ATOM_BUTTON), 0x5001_0001, 7, 67, 50, 14, ""),
    item(2, Class::Atom(ATOM_BUTTON), 0x5001_0000, 62, 67, 50, 14, ""),
    item(3000, Class::Atom(ATOM_STATIC), 0x5002_0000, 7, 7, 190, 10, ""),
    item(1200, Class::Atom(ATOM_EDIT), 0x5081_0080, 7, 18, 190, 13, ""),
    item(3001, Class::Atom(ATOM_STATIC), 0x5002_0000, 7, 36, 190, 10, ""),
    item(1201, Class::Atom(ATOM_EDIT), 0x5081_0080, 7, 47, 171, 13, ""),
    item(1202, Class::Atom(ATOM_BUTTON), 0x5001_0000, 181, 46, 18, 13, "..."),
];

/// The page `id` names, or None when the category has no such template.
pub(super) fn page_def(id: usize) -> Option<PageDef> {
    Some(match id {
        213 => PageDef {
            style: 0x4000_0048,
            w: 255,
            h: 204,
            items: PAGE_213,
        },
        215 => PageDef {
            style: 0x4008_0048,
            w: 250,
            h: 246,
            items: PAGE_215,
        },
        217 => PageDef {
            style: 0x4008_0048,
            w: 267,
            h: 215,
            items: PAGE_217,
        },
        224 => PageDef {
            style: 0x4008_0048,
            w: 291,
            h: 246,
            items: PAGE_224,
        },
        238 => PageDef {
            style: 0x4008_0048,
            w: 250,
            h: 233,
            items: PAGE_238,
        },
        244 => PageDef {
            style: 0x4008_0048,
            w: 250,
            h: 210,
            items: PAGE_244,
        },
        246 => PageDef {
            style: 0x4008_0048,
            w: 250,
            h: 210,
            items: PAGE_246,
        },
        247 => PageDef {
            style: 0x4008_0048,
            w: 250,
            h: 210,
            items: PAGE_247,
        },
        252 => PageDef {
            style: 0x4000_0048,
            w: 257,
            h: 231,
            items: PAGE_252,
        },
        254 => PageDef {
            style: 0x4008_0048,
            w: 250,
            h: 198,
            items: PAGE_254,
        },
        272 => PageDef {
            style: 0x4008_0048,
            w: 266,
            h: 222,
            items: PAGE_272,
        },
        275 => PageDef {
            style: 0x4008_0048,
            w: 272,
            h: 246,
            items: PAGE_275,
        },
        285 => PageDef {
            style: 0x4000_0048,
            w: 272,
            h: 196,
            items: PAGE_285,
        },
        286 => PageDef {
            style: 0x4008_0048,
            w: 250,
            h: 210,
            items: PAGE_286,
        },
        296 => PageDef {
            style: 0x4008_0048,
            w: 293,
            h: 221,
            items: PAGE_296,
        },
        207 => PageDef {
            style: 0x4008_0048,
            w: 250,
            h: 210,
            items: PAGE_207,
        },
        _ => return None,
    })
}

/// The 主机/目录 dialog as a template ready for `DialogBoxIndirectParamW`.
pub(super) fn host_template() -> Vec<u8> {
    build_template(
        "Host/Directory Mapping",
        0x90c8_08cc,
        204,
        88,
        0,
        HOST_ITEMS,
    )
}


#[cfg(test)]
mod tests {
    use super::*;

    /// One control as the dialog manager reads it out of the template.
    #[derive(Debug, PartialEq)]
    struct Read {
        id: i32,
        style: u32,
        rect: (i16, i16, i16, i16),
        class: String,
        text: String,
    }

    struct Walk<'a> {
        b: &'a [u8],
        o: usize,
    }

    impl<'a> Walk<'a> {
        fn u16(&mut self) -> u16 {
            let v = u16::from_le_bytes([self.b[self.o], self.b[self.o + 1]]);
            self.o += 2;
            v
        }

        fn u32(&mut self) -> u32 {
            let v = u32::from_le_bytes(self.b[self.o..self.o + 4].try_into().unwrap());
            self.o += 4;
            v
        }

        fn i16(&mut self) -> i16 {
            self.u16() as i16
        }

        fn skip(&mut self, n: usize) {
            self.o += n;
        }

        /// A template string: absent, a class ordinal, or UTF-16 text.
        fn text(&mut self) -> Option<String> {
            let first = self.u16();
            if first == 0 {
                return None;
            }
            if first == 0xFFFF {
                return Some(format!("#{}", self.u16()));
            }
            let mut units = vec![first];
            loop {
                let u = self.u16();
                if u == 0 {
                    break;
                }
                units.push(u);
            }
            Some(String::from_utf16_lossy(&units))
        }

        fn item(&mut self) -> Read {
            self.o = (self.o + 3) & !3;
            self.skip(8); // helpID and exStyle, both unused here
            let style = self.u32();
            let rect = (self.i16(), self.i16(), self.i16(), self.i16());
            let id = self.u32() as i32;
            let class = self.text().unwrap_or_default();
            let text = self.text().unwrap_or_default();
            let extra = self.u16(); // the count itself is already read
            self.skip(extra as usize); // creation data
            Read {
                id,
                style,
                rect,
                class,
                text,
            }
        }
    }

    /// Reads a built template back the way `CreateDialogIndirectParamW` does,
    /// so a page's geometry is checked without opening a window.  Returns the
    /// controls and the offset the walk stopped at.
    fn controls(tmpl: &[u8]) -> (Vec<Read>, usize) {
        let mut w = Walk { b: tmpl, o: 0 };
        assert_eq!((w.u16(), w.u16()), (1, 0xFFFF), "DLGTEMPLATEEX header");
        w.skip(8); // helpID and exStyle
        let style = w.u32();
        let count = w.u16();
        w.skip(8);
        w.text();
        w.text();
        w.text();
        if style & 0x40 != 0 {
            w.skip(6);
            assert_eq!(w.text().as_deref(), Some("Ms Shell Dlg"));
        }
        let items: Vec<Read> = (0..count).map(|_| w.item()).collect();
        (items, w.o)
    }

    fn check(items: &[Item], tmpl: &[u8], page: usize) {
        let (got, end) = controls(tmpl);
        assert_eq!(end, tmpl.len(), "page {page} trailing bytes");
        assert_eq!(got.len(), items.len(), "page {page} control count");
        for (item, read) in items.iter().zip(&got) {
            let class = match item.class {
                Class::Atom(a) => format!("#{a}"),
                Class::Name(n) => n.to_string(),
            };
            assert_eq!(
                (read.id, read.style, read.rect, &read.class, &read.text),
                (
                    item.id,
                    item.style,
                    (item.x, item.y, item.w, item.h),
                    &class,
                    &item.label.to_string()
                ),
                "page {page} control {}",
                item.id
            );
        }
    }

    #[test]
    fn page_tables_survive_the_template_writer() {
        for id in [213, 215, 217, 224, 238, 244, 246, 247, 252, 254, 272, 275, 285, 286, 296, 207] {
            let def = page_def(id).expect("page");
            let tmpl = build_template("", def.style, def.w, def.h, 0, def.items);
            check(def.items, &tmpl, id);
        }
    }

    #[test]
    fn host_template_survives_the_writer() {
        let tmpl = host_template();
        check(HOST_ITEMS, &tmpl, 273);
    }
}

