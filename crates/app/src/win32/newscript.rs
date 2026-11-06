//! 新建脚本向导（`cm_script_new`, 36013）—— 扩展 > 新建脚本。
//!
//! 三页：**类型、入口与模板** → **名称与信息** → **确认**。一页一组子控件，切页
//! 就是把这组的全部销毁、重建下一组 —— 每页控件少，Tab 顺序天然正确，也
//! 不用为「隐藏的控件还占着焦点环」操心。
//!
//! 第 1 页的三组单选里，**入口**是行为承诺：装载即运行的脚本启动时跑一遍、
//! 不出现在菜单；菜单命令的脚本被包进 `App.command`，以「Lua: 名字」一行
//! 出现在 扩展 菜单底部。
//!
//! 第 2 页的**保存位置**只在引擎启动扫描的根里选（散装两条、脚本包两条，
//! 见 [`spot_choices`]），选错地方脚本不会跑；选择记进 `setting.ini`，
//! 下次打开就是上次的。
//!
//! 形状照 [`pkgdlg`](super::pkgdlg)：`#32770` + 空 `DLGTEMPLATE`（子控件建在
//! `CreateDialogIndirectParamW` 之后），状态对象留在调用方栈上、经
//! `GWLP_USERDATA` 交给 `dlg_proc`，模态由 [`prompt::run_modal_loop`] 驱动，
//! 配色走 [`chrome`]。
//!
//! 向导只收输入，**不写盘**：点「创建」交一份 [`Plan`]（要写的文件和内容），
//! 落盘、装载、开编辑器都是调用方（`window.rs`）的事 —— 和脚本包管理器
//! 「对话框收勾选、调用方落地」是同一条分工。校验在这里做完：到确认页的
//! 每个名字都已查过合法性，撞文件、撞目录挡在交计划那一刻。

#![allow(unsafe_op_in_unsafe_fn)]

use crate::win32::dlggeom;
use dg_render::theme::Theme;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, FillRect, HBRUSH, HDC, HFONT, HGDIOBJ,
};
use windows::Win32::System::SystemServices::SS_ENDELLIPSIS;
use windows::Win32::UI::Controls::{BST_CHECKED, ICC_BAR_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    BM_SETCHECK, BS_AUTORADIOBUTTON, BS_DEFPUSHBUTTON,
    CB_ADDSTRING, CB_GETCURSEL, CB_RESETCONTENT, CB_SETCURSEL,
    CBS_DROPDOWNLIST, CBS_HASSTRINGS,
    CreateDialogIndirectParamW, DLGTEMPLATE, DestroyWindow, ES_AUTOHSCROLL,
    ES_AUTOVSCROLL, ES_MULTILINE, ES_READONLY, GWLP_USERDATA, GetClientRect, GetDlgItem,
    GetWindowLongPtrW, IDCANCEL, PostQuitMessage, SendMessageW,
    SetWindowLongPtrW, SetWindowTextW, WM_CLOSE, WM_COMMAND, WM_CTLCOLOREDIT,
    WM_CTLCOLORBTN, WM_CTLCOLORSTATIC, WM_DESTROY, WM_ERASEBKGND, WM_GETTEXT,
    WM_GETTEXTLENGTH, WM_INITDIALOG, WM_SETFONT, WINDOW_STYLE, WS_BORDER,
    WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN, WS_GROUP, WS_POPUP, WS_SYSMENU, WS_TABSTOP,
    WS_VSCROLL, WS_VISIBLE,
};
use windows::core::PCWSTR;

use super::prompt::{
    CLASS_BUTTON, CLASS_COMBO, CLASS_EDIT, CLASS_STATIC, add_child, run_modal_loop, show_modal,
    wide,
};
use super::window::to_colorref;
use super::{chrome, luascript, uifont};

/// 控件 id。1240 段是本框自己定的 —— 1210 段被脚本包和主题框占着，
/// 错开比撞上便宜。
const ID_KIND_LOOSE: i32 = 1241;
const ID_KIND_PACKAGE: i32 = 1242;
const ID_KIND_NOTE: i32 = 1243;
const ID_TPL_HEAD: i32 = 1244;
const ID_TPL_BASE: i32 = 1245; // 模板四条单选：1245..1248
const ID_TPL_NOTE: i32 = 1249;
const ID_F_NAME: i32 = 1250;
const ID_F_NAME_LABEL: i32 = 1251;
const ID_F_DISPLAY: i32 = 1252;
const ID_F_DISPLAY_LABEL: i32 = 1253;
const ID_F_VERSION: i32 = 1254;
const ID_F_VERSION_LABEL: i32 = 1255;
const ID_F_DESC: i32 = 1256;
const ID_F_DESC_LABEL: i32 = 1257;
const ID_TARGET: i32 = 1258;
const ID_ERROR: i32 = 1259;
const ID_SUMMARY: i32 = 1260;
const ID_FINAL_NOTE: i32 = 1261;
const ID_BACK: i32 = 1262;
const ID_NEXT: i32 = 1263;
const ID_CANCEL: i32 = IDCANCEL.0;
/// 第 1 页的入口单选组：组头、两条单选（1265/1266）和随选中项变化的说明。
const ID_ENTRY_HEAD: i32 = 1264;
const ID_ENTRY_BASE: i32 = 1265;
const ID_ENTRY_NOTE: i32 = 1267;
/// 第 2 页的保存位置：标签 + 下拉（选项见 [`spot_choices`]）。
const ID_LOC_LABEL: i32 = 1268;
const ID_LOC_COMBO: i32 = 1269;

/// 每页自己的控件；切页时逐个销毁。导航按钮三页共用，只换标题。
const PAGE_CONTROLS: [&[i32]; 3] = [
    &[
        ID_KIND_LOOSE,
        ID_KIND_PACKAGE,
        ID_KIND_NOTE,
        ID_ENTRY_HEAD,
        ID_ENTRY_BASE,
        ID_ENTRY_BASE + 1,
        ID_ENTRY_NOTE,
        ID_TPL_HEAD,
        ID_TPL_BASE,
        ID_TPL_BASE + 1,
        ID_TPL_BASE + 2,
        ID_TPL_NOTE,
    ],
    &[
        ID_F_NAME_LABEL,
        ID_F_NAME,
        ID_F_DISPLAY_LABEL,
        ID_F_DISPLAY,
        ID_F_VERSION_LABEL,
        ID_F_VERSION,
        ID_F_DESC_LABEL,
        ID_F_DESC,
        ID_LOC_LABEL,
        ID_LOC_COMBO,
        ID_TARGET,
        ID_ERROR,
    ],
    &[ID_SUMMARY, ID_FINAL_NOTE],
];

/// 每页顶上那行标题。写进窗口标题，不占客户区。
const PAGE_TITLES: [&str; 3] = [
    "新建脚本 — 类型、入口与模板",
    "新建脚本 — 名称与信息",
    "新建脚本 — 确认",
];

/// 客户端尺寸（100% 缩放，px）。宽和高都取脚本包管理器的大半 ——
/// 向导的每页内容不多，大框反而显得空。
const CLIENT_W: i32 = 560;
const CLIENT_H: i32 = 356;

/// 按钮行：底部一排三颗，上一步靠左，下一步和取消靠右。
const RECT_BACK: (i32, i32, i32, i32) = (12, 320, 76, 24);
const RECT_NEXT: (i32, i32, i32, i32) = (356, 320, 96, 24);
const RECT_CANCEL: (i32, i32, i32, i32) = (460, 320, 88, 24);

/// 脚本的两种落点。散装 = `<exe>/scripts/<name>.lua` 一个文件；
/// 包 = 用户包目录下一个带 `package.toml` 的子目录。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Loose,
    Package,
}

/// 类型两行的文案：(单选标题，说明)。
const KIND_CHOICES: [(Kind, &str, &str); 2] = [
    (
        Kind::Loose,
        "散装脚本 — 一个 .lua 文件，放 scripts 目录",
        "装载即运行：顶层代码马上跑一遍；文件改动自动热重载。",
    ),
    (
        Kind::Package,
        "脚本包 — 一个目录，带 package.toml 清单",
        "建在用户包目录；下回启动照清单装载，能带好几个文件。",
    ),
];

/// 脚本怎么被触发。这是行为承诺，不只是代码形状：装载即运行的脚本
/// 没有菜单行，菜单命令的脚本以「Lua: 名字」出现在 扩展 菜单底部。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Entry {
    /// 装载即运行：顶层代码启动装载时执行。
    AutoRun,
    /// 菜单命令：模板正文包进 `App.command`，装载只注册。
    MenuCmd,
}

/// 入口两行的文案：(单选标题，说明)。
const ENTRY_CHOICES: [(Entry, &str, &str); 2] = [
    (
        Entry::AutoRun,
        "装载即运行 — 不出现在菜单",
        "顶层代码启动装载时跑一遍；文件改动自动热重载。",
    ),
    (
        Entry::MenuCmd,
        "菜单命令 — 出现在 扩展 菜单底部",
        "注册为 App.command；装载只注册，点 扩展 菜单底部的「Lua: 名字」或绑定的快捷键才执行。",
    ),
];

/// 模板表：(单选标题，说明)。内容见 [`lua_body`]；入口选「菜单命令」时
/// 任何模板的正文都会被包进 `App.command`，没有例外条款。
const TEMPLATES: [(&str, &str); 3] = [
    ("空脚本", "最小骨架：装载时打一行日志。"),
    ("提示框", "App.alert 弹出当前文档的信息。"),
    ("行处理", "逐行读、改、写回 — 建完就能试。"),
];

/// 向导交出去的结果：要写的文件和创建后的动作。
pub(super) struct Plan {
    /// 装载用的包名。散装 = 文件名主干；包 = 清单 id。
    pub pkg_id: String,
    /// (路径, 内容)，顺序即写入顺序。散装一项；包两项（main.lua、清单）。
    pub files: Vec<(std::path::PathBuf, String)>,
    /// 创建后放进编辑器的那个文件。
    pub open: std::path::PathBuf,
    /// 状态栏一句话。
    pub message: String,
}

/// Windows 的保留设备名 —— 当文件名能建出打不开的文件。
const RESERVED_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// 散装脚本名即文件名：Windows 禁用的字符一个都不能有，保留设备名也不行。
fn check_loose_name(name: &str) -> Result<(), String> {
    let bad = |why: &str| Err(format!("脚本名不合法：{why}"));
    if name.is_empty() {
        return bad("不能是空的");
    }
    if name.len() > 64 {
        return bad("超过 64 个字符");
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return bad("不能以点或空格结尾");
    }
    if name
        .chars()
        .any(|c| matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
    {
        return bad("含有文件名不允许的字符");
    }
    if RESERVED_NAMES.contains(&name.to_ascii_uppercase().as_str()) {
        return bad("是 Windows 的保留设备名");
    }
    Ok(())
}

/// 包 id 和 dg-script 清单同一套规则：小写字母或数字开头，只含
/// `[a-z0-9._-]`，≤64 字符，不含 `..` — 它会变成目录名和 storage 目录名，
/// 所以不能有惊喜（大写、空格、路径分隔符都会在不同的地方咬人）。
fn check_package_id(id: &str) -> Result<(), String> {
    let bad = |why: &str| Err(format!("包 id 不合法：{why}"));
    if id.is_empty() {
        return bad("不能是空的");
    }
    if id.len() > 64 {
        return bad("超过 64 个字符");
    }
    if id.contains("..") {
        return bad("不能含 `..`（它会变成目录名）");
    }
    let first = id.chars().next().expect("非空");
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return bad("首字符要小写字母或数字");
    }
    if id
        .chars()
        .any(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')))
    {
        return bad("只能用 a-z 0-9 . _ -");
    }
    Ok(())
}

/// 版本只挡会把清单写坏的东西：引号、反斜杠、控制符。空了就用默认值。
fn check_version(version: &str) -> Result<(), String> {
    if version
        .chars()
        .any(|c| matches!(c, '"' | '\\' | '\n' | '\r' | '\t'))
    {
        return Err("版本号不能含引号、反斜杠或控制字符".to_string());
    }
    Ok(())
}

/// TOML 基本字符串：转义反斜杠和引号，控制符换空格。
fn toml_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' | '\r' | '\t' => out.push(' '),
            _ => out.push(c),
        }
    }
    out
}

/// 用户包目录：`%LOCALAPPDATA%\DogEdit\packages`（扫描根里的 User 那一份）。
/// 新建的包不该混进随包发布或仓库开发的那两份。
fn user_packages_root() -> Option<std::path::PathBuf> {
    dg_script::package::roots()
        .into_iter()
        .find(|(_, o)| matches!(o, dg_script::package::Origin::User))
        .map(|(p, _)| p)
}

/// 散装/脚本包的可选保存位置 —— 全是引擎启动扫描的根，落在哪个根，
/// 下回启动就从哪个根装载。exe 旁的 `packages`（随包只读）不提供。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Spot {
    /// 程序旁 `scripts`（散装默认）。
    ExeScripts,
    /// 工作目录 `scripts`。
    WorkScripts,
    /// 用户包目录（脚本包默认）。
    UserPackages,
    /// 工作目录 `packages`（开发根，同名包覆盖随包和用户两份）。
    DevPackages,
}

/// 一个类型能选的保存位置，界面顺序即列表顺序。
fn spot_choices(kind: Kind) -> &'static [Spot] {
    match kind {
        Kind::Loose => &[Spot::ExeScripts, Spot::WorkScripts],
        Kind::Package => &[Spot::UserPackages, Spot::DevPackages],
    }
}

impl Spot {
    /// 下拉里的行文案，沿用脚本包管理器「用户 / 开发」的口径。
    fn label(self) -> &'static str {
        match self {
            Spot::ExeScripts => "程序旁 scripts",
            Spot::WorkScripts => "工作目录 scripts",
            Spot::UserPackages => "用户目录 packages",
            Spot::DevPackages => "工作目录 packages",
        }
    }

    /// 是否落在工作目录（确认页「位置：」行加注用）。
    fn in_workdir(self) -> bool {
        matches!(self, Spot::WorkScripts | Spot::DevPackages)
    }
}

/// 位置 → 实际目录。工作目录的两条解析成绝对路径：显示和落盘都
/// 明确，运行时的 cwd 扫描也是同一处。
fn spot_dir(spot: Spot) -> Option<std::path::PathBuf> {
    match spot {
        Spot::ExeScripts => luascript::scripts_dir(),
        Spot::UserPackages => user_packages_root(),
        Spot::WorkScripts => Some(workdir_dir("scripts")),
        Spot::DevPackages => Some(workdir_dir("packages")),
    }
}

/// 工作目录下的 `scripts` / `packages`。引擎扫描用的是相对路径
/// （`package::loose_roots`），这里取 cwd 展开成绝对路径，取不到
/// 时退回相对路径 —— 同一个目录的两种写法。
fn workdir_dir(name: &str) -> std::path::PathBuf {
    std::env::current_dir()
        .map(|d| d.join(name))
        .unwrap_or_else(|_| std::path::PathBuf::from(name))
}

/// 上次选择的持久化键（`setting.ini` 的 `[Scripts]` 段）。没有配置
/// 文件时写入静默放弃 —— 配置文件本来就这个口径。
const SCRIPTS_SECTION: &str = "Scripts";
const KEY_LOOSE_ROOT: &str = "LooseRoot";
const KEY_PKG_ROOT: &str = "PkgRoot";

/// 各类型的默认保存位置：读上次的选择，没配过用程序旁 / 用户目录。
fn spot_default(kind: Kind) -> Spot {
    match kind {
        Kind::Loose => {
            if crate::settings::section(SCRIPTS_SECTION, KEY_LOOSE_ROOT).as_deref() == Some("work") {
                Spot::WorkScripts
            } else {
                Spot::ExeScripts
            }
        }
        Kind::Package => {
            if crate::settings::section(SCRIPTS_SECTION, KEY_PKG_ROOT).as_deref() == Some("dev") {
                Spot::DevPackages
            } else {
                Spot::UserPackages
            }
        }
    }
}

/// 把本次选择记进 `setting.ini`，下次打开向导就是它。
fn remember_spot(kind: Kind, spot: Spot) {
    match kind {
        Kind::Loose => {
            let v = if spot == Spot::WorkScripts { "work" } else { "exe" };
            crate::settings::write_section(SCRIPTS_SECTION, KEY_LOOSE_ROOT, v);
        }
        Kind::Package => {
            let v = if spot == Spot::DevPackages { "dev" } else { "user" };
            crate::settings::write_section(SCRIPTS_SECTION, KEY_PKG_ROOT, v);
        }
    }
}

/// 一份模板的正文。`name` 进注释和示例文案，让创建出来的文件读起来
/// 像是为自己写的。坐标语义和 `trim_trailing.lua` 一致：1 基行号，
/// 1 基 UTF-8 字节列，写回区间是 `[start, end)`。
fn body_tpl(tpl: usize, name: &str) -> String {
    match tpl {
        1 => format!(
            "-- {name}.lua — 提示框示例。\n\
             App.alert((\"当前文档 %q 共 %d 行\"):format(App.doc.name(), App.doc.line_count()))\n"
        ),
        2 => format!(
            "-- {name}.lua — 行处理示例：逐行读，处理完写回。\n\
             -- 列是 UTF-8 字节列（1 基）；#t 与 string.sub 的下标都按字节算。\n\
             \n\
             for i = 1, App.doc.line_count() do\n\
             \x20   local t = App.doc.line(i)\n\
             \x20   -- 示例：去掉行首空白。写回区间是 [start, end)，行尾之后是 #t + 1。\n\
             \x20   local keep = t:match(\"^%s*(.-)%s*$\") or t\n\
             \x20   if #keep ~= #t then\n\
             \x20       App.doc.replace(i, 1, i, #t + 1, keep)\n\
             \x20   end\n\
             end\n\
             App.log(\"{name}: 完成\")\n"
        ),
        // 0 与越界同款：最小骨架。
        _ => format!(
            "-- {name}.lua — 由「新建脚本」向导创建。\n\
             -- 装载即运行：顶层代码马上跑一遍；文件改动会自动热重载。\n\
             \n\
             print(\"{name} 已装载\")\n"
        ),
    }
}

/// 生成文件全文：模板正文按入口落地。装载即运行原样交出；菜单命令把
/// 正文包进 `App.command` —— 装载只注册，点 扩展 菜单底部的菜单行才执行。
fn lua_body(tpl: usize, name: &str, entry: Entry) -> String {
    let body = body_tpl(tpl, name);
    match entry {
        Entry::AutoRun => body,
        Entry::MenuCmd => wrap_in_command(name, &body),
    }
}

/// 把模板正文包进 `App.command`。正文整体缩进一格（空行不缩进），
/// 模板自带的头注释跟着进去，读起来仍是小节标注。
fn wrap_in_command(name: &str, body: &str) -> String {
    let indented: String = body
        .lines()
        .map(|line| {
            if line.trim().is_empty() {
                "\n".to_string()
            } else {
                format!("    {line}\n")
            }
        })
        .collect();
    format!(
        "-- {name}.lua — 由「新建脚本」向导创建。\n\
         -- 入口：扩展菜单命令「Lua: {name}」。装载只注册，点菜单行或绑定的快捷键才执行。\n\
         \n\
         App.command(\"{name}\", function()\n\
         {indented}\
         end)\n"
    )
}

/// 组一份清单文本。`display`/`version`/`description` 走 [`toml_str`]。
fn package_manifest(id: &str, display: &str, version: &str, description: &str) -> String {
    format!(
        "api = 1\nid = \"{}\"\nname = \"{}\"\nversion = \"{}\"\ndescription = \"{}\"\norder = 0\n\n# 想让这个包开机不跑、只在「扩展」菜单占一行点开才启动，就写：\n# launch = \"菜单上的文字\"\nscripts = [\"main.lua\"]\n",
        toml_str(id),
        toml_str(display),
        toml_str(version),
        toml_str(description),
    )
}

/// 组「创建计划」，目录由调用方给 —— 对话框传真目录，测试传临时目录。
/// 名字在这里再做一遍全套校验：合法性、撞文件、撞目录。第 2 页的
/// 实时校验只是提示，这一遍才是过门。
fn build_plan_at(
    kind: Kind,
    entry: Entry,
    tpl: usize,
    name: &str,
    display: &str,
    version: &str,
    description: &str,
    scripts_dir: &std::path::Path,
    packages_root: &std::path::Path,
) -> Result<Plan, String> {
    let name = name.trim();
    // 菜单命令入口在状态栏也报一声 —— 创建完就能去 扩展 菜单找那行。
    let entry_note = if entry == Entry::MenuCmd {
        "，命令在 扩展 菜单底部"
    } else {
        ""
    };
    match kind {
        Kind::Loose => {
            check_loose_name(name)?;
            let path = scripts_dir.join(format!("{name}.lua"));
            if path.exists() {
                return Err(format!("{name}.lua 已经存在，换一个名字"));
            }
            Ok(Plan {
                pkg_id: name.to_string(),
                open: path.clone(),
                files: vec![(path.clone(), lua_body(tpl, name, entry))],
                message: format!("已创建 {} 并装载{entry_note}", path.display()),
            })
        }
        Kind::Package => {
            check_package_id(name)?;
            check_version(version.trim())?;
            let dir = packages_root.join(name);
            if dir.exists() {
                return Err(format!("目录 {} 已经存在，换一个 id", dir.display()));
            }
            let main = dir.join("main.lua");
            let manifest = dir.join("package.toml");
            let display = if display.trim().is_empty() {
                name.to_string()
            } else {
                display.trim().to_string()
            };
            let version = if version.trim().is_empty() {
                "0.1.0"
            } else {
                version.trim()
            };
            Ok(Plan {
                pkg_id: name.to_string(),
                open: main.clone(),
                files: vec![
                    (main.clone(), lua_body(tpl, name, entry)),
                    (
                        manifest.clone(),
                        package_manifest(name, &display, version, description),
                    ),
                ],
                message: format!("已创建脚本包 {name}（main.lua）并装载{entry_note}"),
            })
        }
    }
}

const DLG_STYLE: u32 = WS_POPUP.0 | WS_CAPTION.0 | WS_SYSMENU.0 | WS_CLIPCHILDREN.0;

#[repr(C, align(4))]
struct Template {
    head: DLGTEMPLATE,
    tail: [u16; 3],
}

struct Wizard {
    frame: HWND,
    dpi: i32,
    /// 造框时的主题。模态框活着的时候主题不会变，留一份原样的 ——
    /// 切页重建控件时靠它重问一遍子控件的主题。
    theme: Theme,
    font: HFONT,
    bg: HBRUSH,
    bg_ref: COLORREF,
    ink: COLORREF,
    line: HBRUSH,
    /// 编辑框的底：比对话框面浅一档的输入区色（`toolbar_field`），深色
    /// 主题下不处理 `WM_CTLCOLOREDIT` 的框顶着一排白格子。
    field: HBRUSH,
    field_face: COLORREF,
    page: usize,
    /// 当前**建着**的那一页。切页先拆它再建新页 —— 拆 [`PAGE_CONTROLS`]
    /// 的哪张表必须对着已经存在的控件，跟着 `page` 走就拆错了。
    built: usize,
    kind: Kind,
    /// 入口方式：行为承诺，进代码生成（[`lua_body`]）和确认页摘要。
    entry: Entry,
    /// 保存位置：[`spot_choices`] 里的一条，随类型走，切类型时重置
    /// 为该类型的默认。
    spot: Spot,
    tpl: usize,
    /// 第 2 页的字段镜像：离开那页时收进来，回来时填回去，
    /// 确认页的摘要也从这里组。
    name: String,
    display: String,
    version: String,
    description: String,
    result: Option<Plan>,
    destroyed: bool,
}

unsafe fn state_mut(hwnd: HWND) -> &'static mut Wizard {
    &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Wizard)
}

/// 模态跑一遍：`Some(plan)` = 点了创建，`None` = 取消。
pub(super) fn show(frame: HWND, theme: &Theme) -> Option<Plan> {
    let dpi = unsafe { uifont::window_dpi(frame) };
    let mut st = Wizard {
        frame,
        dpi,
        theme: theme.clone(),
        font: unsafe { uifont::create(dpi) },
        bg: unsafe { CreateSolidBrush(to_colorref(theme.background)) },
        bg_ref: to_colorref(theme.background),
        ink: to_colorref(theme.foreground),
        line: unsafe { CreateSolidBrush(to_colorref(theme.indent_line)) },
        field: unsafe { CreateSolidBrush(to_colorref(theme.toolbar_field)) },
        field_face: to_colorref(theme.toolbar_field),
        page: 0,
        built: 0,
        kind: Kind::Loose,
        entry: Entry::AutoRun,
        spot: spot_default(Kind::Loose),
        tpl: 0,
        name: String::new(),
        display: String::new(),
        version: String::from("0.1.0"),
        description: String::new(),
        result: None,
        destroyed: false,
    };
    unsafe { run(&mut st) };
    unsafe {
        let _ = DeleteObject(HGDIOBJ(st.font.0));
        let _ = DeleteObject(HGDIOBJ(st.bg.0));
        let _ = DeleteObject(HGDIOBJ(st.line.0));
        let _ = DeleteObject(HGDIOBJ(st.field.0));
    }
    st.result
}

unsafe fn run(st: &mut Wizard) {
    let _ = InitCommonControlsEx(&INITCOMMONCONTROLSEX {
        dwICC: ICC_BAR_CLASSES,
        ..Default::default()
    });
    let tmpl = Template {
        head: DLGTEMPLATE {
            style: DLG_STYLE,
            dwExtendedStyle: 0,
            cdit: 0,
            x: 0,
            y: 0,
            cx: 64,
            cy: 64,
        },
        tail: [0, 0, 0],
    };
    let param = LPARAM(st as *mut Wizard as isize);
    let Ok(dlg) = CreateDialogIndirectParamW(
        Some(super::view::instance()),
        &tmpl.head,
        Some(st.frame),
        Some(dlg_proc),
        param,
    ) else {
        return;
    };

    chrome::set_chrome_theme(&st.theme);
    chrome::title_bar(dlg, chrome::theme_is_dark(&st.theme));

    // 导航按钮三页共用，建一次。下一步是默认钮：回车永远走主路。
    let dpi = st.dpi as f32 / 96.0;
    for (id, label, style, rect) in [
        (ID_BACK, "上一步(&B)", WS_TABSTOP, RECT_BACK),
        (ID_NEXT, "下一步(&X) >", default_button(), RECT_NEXT),
        (ID_CANCEL, "取消", WS_TABSTOP, RECT_CANCEL),
    ] {
        let label = crate::win32::lang::trs(label);
        let _ = child(dlg, st, CLASS_BUTTON, &label, id, style, rect, dpi);
    }

    build_page(dlg, st);
    size_and_place(dlg, st);

    show_modal(dlg, st.frame);
    focus_first(dlg);
    run_modal_loop(dlg, st.frame, std::ptr::addr_of!(st.destroyed));
}

impl Wizard {
    // 配色相关的判断都从留着的这份主题来：深/浅、子控件的主题重刷，
    // 问的都是 `theme_is_dark(&st.theme)`，不再有别的来源。
}

fn default_button() -> WINDOW_STYLE {
    WINDOW_STYLE(WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32)
}

#[allow(clippy::too_many_arguments)]
unsafe fn child(
    dlg: HWND,
    st: &Wizard,
    class: PCWSTR,
    text: &str,
    id: i32,
    style: WINDOW_STYLE,
    rect: (i32, i32, i32, i32),
    dpi: f32,
) -> Option<HWND> {
    let w = wide(text);
    let ctl = add_child(dlg, class, PCWSTR::from_raw(w.as_ptr()), id, style, rect, dpi)?;
    let _ = SendMessageW(
        ctl,
        WM_SETFONT,
        Some(WPARAM(st.font.0 as usize)),
        Some(LPARAM(1)),
    );
    Some(ctl)
}

/// 销毁一页的全部控件，再建下一页的；顺手把标题、按钮文案和可用态换掉。
/// 新建的控件要重新过一遍深色主题 —— `SetWindowTheme` 是每个窗口自己的事。
unsafe fn build_page(dlg: HWND, st: &mut Wizard) {
    // 拆的是 `built`（当前建着的那页），不是 `page`（要去的这页）——
    // 到这里时 `page` 已经指过去了，拆它的表等于什么都没拆，旧控件全数
    // 留在新页底下（第一次实测：模板的单选和第 2 页的文字叠在一起）。
    for id in PAGE_CONTROLS[st.built] {
        if let Ok(ctl) = GetDlgItem(Some(dlg), *id) {
            let _ = DestroyWindow(ctl);
        }
    }
    st.built = st.page;
    let dpi = st.dpi as f32 / 96.0;
    match st.page {
        0 => build_page0(dlg, st, dpi),
        1 => build_page1(dlg, st, dpi),
        _ => build_page2(dlg, st, dpi),
    }
    chrome::dark_theme_children(dlg, chrome::theme_is_dark(&st.theme));
    let _ = SetWindowTextW(dlg, PCWSTR::from_raw(wide(PAGE_TITLES[st.page]).as_ptr()));
    // 最后一页的主按钮叫「创建」。
    let next = if st.page == 2 { "创建(&C)" } else { "下一步(&X) >" };
    if let Ok(ctl) = GetDlgItem(Some(dlg), ID_NEXT) {
        set_text(ctl, &crate::win32::lang::trs(next));
    }
    // 上一步在第 1 页没有去处。
    if let Ok(ctl) = GetDlgItem(Some(dlg), ID_BACK) {
        let _ = EnableWindow(ctl, st.page > 0);
    }
}

/// 第 1 页：类型、入口、模板三组单选，各带一行随选中项变化的说明。
/// 入口是行为承诺（见模块头）；它与模板正交 —— 「菜单命令」下任何
/// 模板的正文都包进 `App.command`，模板组没有可用态要摆。
unsafe fn build_page0(dlg: HWND, st: &Wizard, dpi: f32) {
    // 两组单选各自 WS_GROUP：方向键在组内换选项，Tab 在组间跳。
    for (i, (_, title, _)) in KIND_CHOICES.iter().enumerate() {
        let style = WINDOW_STYLE(
            (WS_CHILD | WS_VISIBLE | WS_TABSTOP).0
                | BS_AUTORADIOBUTTON as u32
                | if i == 0 { WS_GROUP.0 } else { 0 },
        );
        let rect = (24, 40 + i as i32 * 24, 520, 18);
        let _ = child(
            dlg,
            st,
            CLASS_BUTTON,
            &crate::win32::lang::trs(title),
            ID_KIND_LOOSE + i as i32,
            style,
            rect,
            dpi,
        );
        set_radio(dlg, ID_KIND_LOOSE + i as i32, st.kind == KIND_CHOICES[i].0);
    }
    let _ = child(
        dlg,
        st,
        CLASS_STATIC,
        &KIND_CHOICES[kind_index(st.kind)].2,
        ID_KIND_NOTE,
        ellipsis(),
        (24, 88, 520, 16),
        dpi,
    );
    let _ = child(
        dlg,
        st,
        CLASS_STATIC,
        "入口",
        ID_ENTRY_HEAD,
        WINDOW_STYLE::default(),
        (24, 112, 520, 16),
        dpi,
    );
    for (i, (_, title, _)) in ENTRY_CHOICES.iter().enumerate() {
        let style = WINDOW_STYLE(
            (WS_CHILD | WS_VISIBLE | WS_TABSTOP).0
                | BS_AUTORADIOBUTTON as u32
                | if i == 0 { WS_GROUP.0 } else { 0 },
        );
        let rect = (24, 134 + i as i32 * 22, 520, 18);
        let _ = child(
            dlg,
            st,
            CLASS_BUTTON,
            &crate::win32::lang::trs(title),
            ID_ENTRY_BASE + i as i32,
            style,
            rect,
            dpi,
        );
        set_radio(dlg, ID_ENTRY_BASE + i as i32, st.entry == ENTRY_CHOICES[i].0);
    }
    let _ = child(
        dlg,
        st,
        CLASS_STATIC,
        &ENTRY_CHOICES[entry_index(st.entry)].2,
        ID_ENTRY_NOTE,
        ellipsis(),
        (24, 180, 520, 16),
        dpi,
    );
    let _ = child(
        dlg,
        st,
        CLASS_STATIC,
        "模板",
        ID_TPL_HEAD,
        WINDOW_STYLE::default(),
        (24, 204, 520, 16),
        dpi,
    );
    for (i, (title, _)) in TEMPLATES.iter().enumerate() {
        let style = WINDOW_STYLE(
            (WS_CHILD | WS_VISIBLE | WS_TABSTOP).0
                | BS_AUTORADIOBUTTON as u32
                | if i == 0 { WS_GROUP.0 } else { 0 },
        );
        let rect = (24, 226 + i as i32 * 24, 520, 18);
        let _ = child(
            dlg,
            st,
            CLASS_BUTTON,
            &crate::win32::lang::trs(title),
            ID_TPL_BASE + i as i32,
            style,
            rect,
            dpi,
        );
        set_radio(dlg, ID_TPL_BASE + i as i32, st.tpl == i);
    }
    let _ = child(
        dlg,
        st,
        CLASS_STATIC,
        &TEMPLATES[st.tpl].1,
        ID_TPL_NOTE,
        ellipsis(),
        (24, 296, 520, 16),
        dpi,
    );
}

/// 第 2 页：名称 + 包才有的三格 + 保存位置下拉。目标行跟着输入和
/// 位置走，错误行只在有错时出字。
unsafe fn build_page1(dlg: HWND, st: &Wizard, dpi: f32) {
    let edit_style = WINDOW_STYLE(
        (WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER).0 | ES_AUTOHSCROLL as u32,
    );
    let pkg = st.kind == Kind::Package;
    let name_label = if pkg { "包 id" } else { "脚本名" };
    let _ = child(
        dlg,
        st,
        CLASS_STATIC,
        &format!("{}:", crate::win32::lang::trs(name_label)),
        ID_F_NAME_LABEL,
        WINDOW_STYLE::default(),
        (12, 42, 100, 16),
        dpi,
    );
    let _ = child(
        dlg,
        st,
        CLASS_EDIT,
        "",
        ID_F_NAME,
        edit_style,
        (120, 40, 320, 22),
        dpi,
    );
    let mut next_y = 76;
    if pkg {
        for (id_l, id_e, label) in [
            (ID_F_DISPLAY_LABEL, ID_F_DISPLAY, "显示名"),
            (ID_F_VERSION_LABEL, ID_F_VERSION, "版本"),
            (ID_F_DESC_LABEL, ID_F_DESC, "描述"),
        ] {
            let _ = child(
                dlg,
                st,
                CLASS_STATIC,
                &format!("{}:", crate::win32::lang::trs(label)),
                id_l,
                WINDOW_STYLE::default(),
                (12, next_y + 2, 100, 16),
                dpi,
            );
            let _ = child(
                dlg,
                st,
                CLASS_EDIT,
                "",
                id_e,
                edit_style,
                (120, next_y, 320, 22),
                dpi,
            );
            next_y += 32;
        }
    }
    fill_page1(dlg, st);
    // 保存位置：两条引擎真正扫描的根（[`spot_choices`]），选完目标行
    // 立即跟上。下拉的 rect 高度含展开区，照 tooldlg 的组合框口径；
    // 下面的目标行把这段高度让足。
    let _ = child(
        dlg,
        st,
        CLASS_STATIC,
        &format!("{}:", crate::win32::lang::trs("保存位置")),
        ID_LOC_LABEL,
        WINDOW_STYLE::default(),
        (12, next_y + 4, 100, 16),
        dpi,
    );
    let _ = child(
        dlg,
        st,
        CLASS_COMBO,
        "",
        ID_LOC_COMBO,
        WINDOW_STYLE(WS_TABSTOP.0 | CBS_DROPDOWNLIST as u32 | CBS_HASSTRINGS as u32),
        (120, next_y, 320, 48),
        dpi,
    );
    fill_spot_combo(dlg, st);
    let _ = child(
        dlg,
        st,
        CLASS_STATIC,
        "",
        ID_TARGET,
        ellipsis(),
        (12, next_y + 56, 536, 16),
        dpi,
    );
    let _ = child(
        dlg,
        st,
        CLASS_STATIC,
        "",
        ID_ERROR,
        ellipsis(),
        (12, next_y + 82, 536, 16),
        dpi,
    );
    refresh_page1(dlg, st);
    if let Ok(name) = GetDlgItem(Some(dlg), ID_F_NAME) {
        let _ = SetFocus(Some(name));
    }
}

/// 把镜像里的字段填回第 2 页 —— 回退再前进时不丢用户输入。
unsafe fn fill_page1(dlg: HWND, st: &Wizard) {
    set_edit(dlg, ID_F_NAME, &st.name);
    if st.kind == Kind::Package {
        set_edit(dlg, ID_F_DISPLAY, &st.display);
        set_edit(dlg, ID_F_VERSION, &st.version);
        set_edit(dlg, ID_F_DESC, &st.description);
    }
}

/// 把当前类型可用的保存位置填进下拉，选中 [`Wizard::spot`]。
unsafe fn fill_spot_combo(dlg: HWND, st: &Wizard) {
    let Ok(combo) = GetDlgItem(Some(dlg), ID_LOC_COMBO) else {
        return;
    };
    let _ = SendMessageW(combo, CB_RESETCONTENT, None, None);
    for spot in spot_choices(st.kind) {
        let text = wide(spot.label());
        let _ = SendMessageW(combo, CB_ADDSTRING, None, Some(LPARAM(text.as_ptr() as isize)));
    }
    let cur = spot_choices(st.kind)
        .iter()
        .position(|s| *s == st.spot)
        .unwrap_or(0);
    let _ = SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(cur)), None);
}

/// 下拉当前选中项；空列表（没有选中）时 None。照 tooldlg 的口径。
unsafe fn combo_index(dlg: HWND, id: i32) -> Option<usize> {
    let ctl = GetDlgItem(Some(dlg), id).ok()?;
    let i = SendMessageW(ctl, CB_GETCURSEL, None, None).0;
    if i < 0 { None } else { Some(i as usize) }
}

/// 第 2 页的活行：目标路径 + 校验错误。名称框 `EN_CHANGE`、位置下拉
/// 一跳就来一遍。
unsafe fn refresh_page1(dlg: HWND, st: &Wizard) {
    let name = edit_text_of(dlg, ID_F_NAME);
    let pkg = st.kind == Kind::Package;
    let dir = spot_dir(st.spot);
    let target = match (pkg, dir) {
        (false, Some(dir)) => {
            if name.trim().is_empty() {
                format!("将创建 {}\\<脚本名>.lua", dir.display())
            } else {
                format!("将创建 {}\\{}.lua", dir.display(), name.trim())
            }
        }
        (true, Some(root)) => {
            if name.trim().is_empty() {
                format!("将创建 {}\\<包id>\\package.toml 和 main.lua", root.display())
            } else {
                format!(
                    "将创建 {}\\{}\\package.toml 和 main.lua",
                    root.display(),
                    name.trim()
                )
            }
        }
        _ => "目标目录未知".to_string(),
    };
    if let Some(ctl) = opt_item(dlg, ID_TARGET) {
        set_text(ctl, &target);
    }
    let error = validate_page1(st, &name).err().unwrap_or_default();
    if let Some(ctl) = opt_item(dlg, ID_ERROR) {
        set_text(ctl, &error);
    }
}

/// 第 2 页的实时校验。只管「现在填的这行能不能放行」，撞文件留给
/// [`build_plan_at`]（它看的才是最终路径）。版本用的是镜像 —— 用户在
/// 那一格打字的瞬间不重查，点下一步时 [`build_plan_at`] 会再查一遍。
fn validate_page1(st: &Wizard, name: &str) -> Result<(), String> {
    let name = name.trim();
    if st.kind == Kind::Loose {
        return check_loose_name(name);
    }
    check_package_id(name)?;
    check_version(&st.version)
}

/// 第 3 页：只读的多行摘要 + 一句「点创建会发生什么」。
unsafe fn build_page2(dlg: HWND, st: &Wizard, dpi: f32) {
    let summary = summary_text(st);
    let style = WINDOW_STYLE(
        (WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER | WS_VSCROLL).0
            | (ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32,
    );
    let _ = child(dlg, st, CLASS_EDIT, "", ID_SUMMARY, style, (12, 36, 536, 232), dpi);
    if let Some(ctl) = opt_item(dlg, ID_SUMMARY) {
        set_text(ctl, &crlf(&summary));
    }
    let _ = child(
        dlg,
        st,
        CLASS_STATIC,
        "点「创建」：写文件 → 立即装载 → 主文件放进编辑器，保存即热重载。",
        ID_FINAL_NOTE,
        ellipsis(),
        (12, 276, 536, 16),
        dpi,
    );
}

/// Edit 控件的换行是 `\r\n`：`summary_text` 组的 `\n` 直接给它会把
/// 整份摘要并成一段。
fn crlf(s: &str) -> String {
    s.replace('\n', "\r\n")
}

/// 确认页的摘要：类型、落点、入口、要写的文件、装载后的效果。每行都从
/// `Plan` 的来源数据组出来，让用户确认的就是将要发生的事。入口行把
/// 「会不会有菜单行」说成一句可扫读的结论 —— 那是用户最容易意外的部分。
fn summary_text(st: &Wizard) -> String {
    let dir = spot_dir(st.spot);
    // 工作目录的根加一句注 —— 和程序旁的 scripts 同名，不注会看混。
    let note = if st.spot.in_workdir() { "（工作目录）" } else { "" };
    let (kind_line, dir_line) = match st.kind {
        Kind::Loose => (
            "类型：散装脚本（一个 .lua 文件）".to_string(),
            dir.map(|d| format!("位置：{}{note}", d.display()))
                .unwrap_or_else(|| "位置：未知".to_string()),
        ),
        Kind::Package => (
            "类型：脚本包（package.toml + main.lua）".to_string(),
            dir.map(|d| format!("位置：{}\\{}{note}", d.display(), st.name.trim()))
                .unwrap_or_else(|| "位置：未知".to_string()),
        ),
    };
    let tpl = format!("模板：{}（{}）", TEMPLATES[st.tpl].0, TEMPLATES[st.tpl].1);
    let entry_line = match st.entry {
        Entry::AutoRun => "入口：装载即运行 —— 启动装载时执行，不出现在菜单".to_string(),
        Entry::MenuCmd => format!(
            "入口：菜单命令 —— 扩展 菜单底部的「Lua: {}」",
            st.name.trim()
        ),
    };
    let mut out = format!("{kind_line}\n{dir_line}\n{tpl}\n{entry_line}\n\n将创建：\n");
    for path in preview_files(st) {
        out.push_str(&format!("  {}\n", path.display()));
    }
    let tail = if st.entry == Entry::MenuCmd {
        "创建后立即装载：注册的命令出现在 扩展 菜单底部。"
    } else {
        "创建后立即装载，之后改动热重载。"
    };
    out.push_str(&format!("\n{tail}\n"));
    out
}

/// 确认页预览的文件清单 —— 和 [`build_plan_at`] 同一套目录，但不查
/// 冲突（冲突是点「创建」那一刻的事）。
fn preview_files(st: &Wizard) -> Vec<std::path::PathBuf> {
    let name = st.name.trim();
    match st.kind {
        Kind::Loose => spot_dir(st.spot)
            .map(|d| vec![d.join(format!("{name}.lua"))])
            .unwrap_or_default(),
        Kind::Package => spot_dir(st.spot)
            .map(|root| {
                let dir = root.join(name);
                vec![dir.join("main.lua"), dir.join("package.toml")]
            })
            .unwrap_or_default(),
    }
}

/// 收第 2 页的输入进镜像（离开那页时调）。
unsafe fn collect_page1(dlg: HWND, st: &mut Wizard) {
    st.name = edit_text_of(dlg, ID_F_NAME);
    if st.kind == Kind::Package {
        st.display = edit_text_of(dlg, ID_F_DISPLAY);
        st.version = edit_text_of(dlg, ID_F_VERSION);
        st.description = edit_text_of(dlg, ID_F_DESC);
    }
}

/// 单选的状态。两组 `WS_GROUP` 各管各的，设谁谁亮，同组其余自灭。
unsafe fn set_radio(dlg: HWND, id: i32, on: bool) {
    if let Ok(ctl) = GetDlgItem(Some(dlg), id) {
        let _ = SendMessageW(
            ctl,
            BM_SETCHECK,
            Some(WPARAM(if on { BST_CHECKED.0 as usize } else { 0 })),
            None,
        );
    }
}

unsafe fn edit_text_of(dlg: HWND, id: i32) -> String {
    let Ok(ctl) = GetDlgItem(Some(dlg), id) else {
        return String::new();
    };
    let len = SendMessageW(ctl, WM_GETTEXTLENGTH, None, None).0 as usize;
    if len == 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len + 1];
    let got = SendMessageW(
        ctl,
        WM_GETTEXT,
        Some(WPARAM(len + 1)),
        Some(LPARAM(buf.as_mut_ptr() as isize)),
    )
    .0 as usize;
    String::from_utf16_lossy(&buf[..got.min(len)])
}

fn ellipsis() -> WINDOW_STYLE {
    WINDOW_STYLE(SS_ENDELLIPSIS.0)
}

unsafe fn opt_item(dlg: HWND, id: i32) -> Option<HWND> {
    GetDlgItem(Some(dlg), id).ok()
}

unsafe fn set_text(ctl: HWND, text: &str) {
    let buf: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let _ = SetWindowTextW(ctl, PCWSTR::from_raw(buf.as_ptr()));
}

unsafe fn set_edit(dlg: HWND, id: i32, text: &str) {
    if let Some(ctl) = opt_item(dlg, id) {
        set_text(ctl, text);
    }
}

/// 焦点给这页第一个能收的控件：第 1 页是「散装脚本」那条单选，第 2 页
/// [`build_page1`] 自己给了名称框，第 3 页给摘要（滚到顶）。
unsafe fn focus_first(dlg: HWND) {
    let base = match state_mut(dlg).page {
        0 => ID_KIND_LOOSE,
        2 => ID_SUMMARY,
        _ => return,
    };
    if let Ok(ctl) = GetDlgItem(Some(dlg), base) {
        let _ = SetFocus(Some(ctl));
    }
}

/// 下一步：第 2 页先收输入并校验，挡住带错的输入；第 3 页就是创建。
unsafe fn advance(dlg: HWND) {
    let st = state_mut(dlg);
    if st.page == 1 {
        let name = edit_text_of(dlg, ID_F_NAME);
        if let Err(why) = validate_page1(st, &name) {
            if let Some(ctl) = opt_item(dlg, ID_ERROR) {
                set_text(ctl, &why);
            }
            return;
        }
        collect_page1(dlg, st);
    }
    if st.page < 2 {
        st.page += 1;
        build_page(dlg, st);
        focus_first(dlg);
        return;
    }
    // 创建：组计划，成了挂进出口；失败把原因写回第 2 页的错误行，弹回去
    // 让用户改。
    let st = state_mut(dlg);
    let built = match st.kind {
        Kind::Loose => spot_dir(st.spot)
            .ok_or_else(|| "找不到脚本目录".to_string())
            .and_then(|dir| {
                build_plan_at(
                    st.kind,
                    st.entry,
                    st.tpl,
                    &st.name,
                    "",
                    "",
                    "",
                    &dir,
                    std::path::Path::new(""),
                )
            }),
        Kind::Package => spot_dir(st.spot)
            .ok_or_else(|| "找不到用户包目录".to_string())
            .and_then(|root| {
                build_plan_at(
                    st.kind,
                    st.entry,
                    st.tpl,
                    &st.name,
                    &st.display,
                    &st.version,
                    &st.description,
                    std::path::Path::new(""),
                    &root,
                )
            }),
    };
    match built {
        Ok(plan) => {
            // 记住这次的保存位置，下次打开向导就是它。
            remember_spot(st.kind, st.spot);
            state_mut(dlg).result = Some(plan);
            let _ = DestroyWindow(dlg);
        }
        Err(why) => {
            let st = state_mut(dlg);
            st.page = 1;
            build_page(dlg, st);
            if let Some(ctl) = opt_item(dlg, ID_ERROR) {
                set_text(ctl, &why);
            }
        }
    }
}

/// 上一步：第 2 页的输入先收走，页码往回退一格。
unsafe fn go_back(dlg: HWND) {
    let st = state_mut(dlg);
    if st.page == 1 {
        collect_page1(dlg, st);
    }
    if st.page > 0 {
        st.page -= 1;
        build_page(dlg, st);
        focus_first(dlg);
    }
}

fn kind_index(kind: Kind) -> usize {
    if kind == Kind::Loose { 0 } else { 1 }
}

fn entry_index(entry: Entry) -> usize {
    if entry == Entry::AutoRun { 0 } else { 1 }
}

unsafe fn on_command(dlg: HWND, wparam: WPARAM) -> isize {
    let id = (wparam.0 & 0xFFFF) as u16 as i32;
    match id {
        ID_BACK => {
            go_back(dlg);
            1
        }
        ID_NEXT => {
            advance(dlg);
            1
        }
        ID_CANCEL => {
            let _ = DestroyWindow(dlg);
            1
        }
        ID_KIND_LOOSE | ID_KIND_PACKAGE => {
            // 切类型：镜像跟着换，说明行换文案。保存位置也重置成该类型
            // 的默认 —— 两种类型各有一套根。第 2 页的字段表要等真到
            // 了那页才重建（[`build_page1`] 按当前类型排格）。
            let st = state_mut(dlg);
            st.kind = if id == ID_KIND_LOOSE {
                Kind::Loose
            } else {
                Kind::Package
            };
            st.spot = spot_default(st.kind);
            if let Some(ctl) = opt_item(dlg, ID_KIND_NOTE) {
                set_text(ctl, &KIND_CHOICES[kind_index(st.kind)].2);
            }
            1
        }
        id if (ID_ENTRY_BASE..=ID_ENTRY_BASE + 1).contains(&id) => {
            // 切入口：镜像跟着换，说明行换文案。模板组与入口正交，没有
            // 可用态要重摆。
            let st = state_mut(dlg);
            st.entry = if id == ID_ENTRY_BASE {
                Entry::AutoRun
            } else {
                Entry::MenuCmd
            };
            if let Some(ctl) = opt_item(dlg, ID_ENTRY_NOTE) {
                set_text(ctl, &ENTRY_CHOICES[entry_index(st.entry)].2);
            }
            1
        }
        id if (ID_TPL_BASE..=ID_TPL_BASE + 2).contains(&id) => {
            let st = state_mut(dlg);
            st.tpl = (id - ID_TPL_BASE) as usize;
            if let Some(ctl) = opt_item(dlg, ID_TPL_NOTE) {
                set_text(ctl, &TEMPLATES[st.tpl].1);
            }
            1
        }
        // 名称改了就刷目标行和错误行。
        ID_F_NAME => {
            refresh_page1(dlg, state_mut(dlg));
            1
        }
        // 保存位置换了也刷 —— 目标行立即跟上。
        ID_LOC_COMBO => {
            let st = state_mut(dlg);
            if let Some(i) = combo_index(dlg, ID_LOC_COMBO)
                && let Some(spot) = spot_choices(st.kind).get(i)
            {
                st.spot = *spot;
                refresh_page1(dlg, st);
            }
            1
        }
        _ => 0,
    }
}

unsafe extern "system" fn dlg_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> isize {
    match msg {
        WM_INITDIALOG => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam.0);
            1
        }
        WM_COMMAND => on_command(hwnd, wparam),
        WM_ERASEBKGND => {
            // `#32770` 自带 `COLOR_3DFACE` 刷子，控件之间和背后的底色得自己
            // 从 Theme 出。
            let st = state_mut(hwnd);
            let mut rc = RECT::default();
            if GetClientRect(hwnd, &mut rc).is_ok() {
                let _ = FillRect(HDC(wparam.0 as *mut _), &rc, st.bg);
            }
            1
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let st = state_mut(hwnd);
            super::customdraw::ctlcolor(HDC(wparam.0 as *mut _), st.ink, st.bg_ref, st.bg)
        }
        // 名称/显示名/版本/描述的输入格：底色换输入区色，不处理这条的
        // 框在深色主题下顶着白格子。只读的摘要框走上面 STATIC 那条
        // （`ES_READONLY` 的 edit 发的是 `WM_CTLCOLORSTATIC`）。
        WM_CTLCOLOREDIT => {
            let st = state_mut(hwnd);
            super::customdraw::ctlcolor(HDC(wparam.0 as *mut _), st.ink, st.field_face, st.field)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            1
        }
        WM_DESTROY => {
            state_mut(hwnd).destroyed = true;
            PostQuitMessage(0);
            0
        }
        _ => 0,
    }
}

/// 外框尺寸按客户端算，再居中压在编辑区上 —— 取高度的 1/3 而不是 1/2，
/// 和脚本包管理器一样，对话框偏上更像原生。
unsafe fn size_and_place(dlg: HWND, st: &Wizard) {
    let dpi = st.dpi;
    dlggeom::size_and_place(
        dlg,
        st.frame,
        uifont::scaled(CLIENT_W, dpi),
        uifont::scaled(CLIENT_H, dpi),
        dpi as u32,
        DLG_STYLE,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wizard(kind: Kind, tpl: usize, name: &str) -> Wizard {
        Wizard {
            frame: HWND::default(),
            dpi: 96,
            theme: Theme::default(),
            font: HFONT::default(),
            bg: HBRUSH::default(),
            bg_ref: COLORREF(0),
            ink: COLORREF(0),
            line: HBRUSH::default(),
            field: HBRUSH::default(),
            field_face: COLORREF(0),
            page: 2,
            built: 2,
            kind,
            entry: Entry::AutoRun,
            spot: if kind == Kind::Package {
                Spot::UserPackages
            } else {
                Spot::ExeScripts
            },
            tpl,
            name: name.to_string(),
            display: String::new(),
            version: String::new(),
            description: String::new(),
            result: None,
            destroyed: false,
        }
    }

    /// 网格是手算的：按钮不出界、不叠。
    #[test]
    fn the_grid_fits_the_client() {
        for (x, y, w, h) in [RECT_BACK, RECT_NEXT, RECT_CANCEL] {
            assert!(x + w <= CLIENT_W, "({x},{y},{w},{h}) 超出宽度");
            assert!(y + h <= CLIENT_H, "({x},{y},{w},{h}) 超出高度");
        }
        let mut buttons = [RECT_BACK, RECT_NEXT, RECT_CANCEL];
        buttons.sort_by_key(|r| r.0);
        for pair in buttons.windows(2) {
            assert!(pair[0].0 + pair[0].2 <= pair[1].0, "{pair:?} 重叠");
        }
        // 每页最靠下的行都在按钮行之上。
        for (x, y, w, h) in [
            (24, 296, 520, 16), // 第 1 页的模板说明
            (12, 254, 536, 16), // 第 2 页（包模式）的错误行
            (12, 276, 536, 16), // 第 3 页的结语
        ] {
            assert!(y + h <= RECT_BACK.1, "({x},{y},{w},{h}) 压到按钮行");
            assert!(x + w <= CLIENT_W);
        }
    }

    /// 控件 id 互不撞，也不压到 IDOK(1)/IDCANCEL(2) 上 —— 导航按钮
    /// 1241 起步，天然错开；页表才是要盯的那份。
    #[test]
    fn control_ids_do_not_collide() {
        let mut ids: Vec<i32> = PAGE_CONTROLS
            .iter()
            .flat_map(|page| page.iter().copied())
            .chain([ID_BACK, ID_NEXT])
            .collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "控件 id 必须唯一");
        assert!(ids.iter().all(|&id| id != 1 && id != 2), "IDOK/IDCANCEL");
    }

    /// 散装脚本名：Windows 文件名的底线。
    #[test]
    fn loose_names_obey_the_filename_rules() {
        assert!(check_loose_name("trim_all").is_ok());
        assert!(check_loose_name("小写不行吗").is_ok(), "中文文件名合法");
        assert!(check_loose_name("").is_err());
        assert!(check_loose_name("a/b").is_err());
        assert!(check_loose_name("a\\b").is_err());
        assert!(check_loose_name("a:b").is_err());
        assert!(check_loose_name("a*b").is_err());
        assert!(check_loose_name("trail.").is_err());
        assert!(check_loose_name("trail ").is_err());
        assert!(check_loose_name("CON").is_err());
        assert!(check_loose_name("com1").is_err(), "保留名不分大小写");
        assert!(check_loose_name(&"x".repeat(65)).is_err());
    }

    /// 包 id：和 dg-script 清单同一套规则。
    #[test]
    fn package_ids_obey_the_manifest_rules() {
        assert!(check_package_id("hello").is_ok());
        assert!(check_package_id("a1.b_c-d").is_ok());
        assert!(check_package_id("").is_err());
        assert!(check_package_id("Hello").is_err(), "大写不行");
        assert!(check_package_id("1abc").is_ok(), "数字开头可以");
        assert!(check_package_id(".abc").is_err());
        assert!(check_package_id("a..b").is_err());
        assert!(check_package_id("a b").is_err());
        assert!(check_package_id(&"x".repeat(65)).is_err());
    }

    /// 版本号只挡会写坏清单的字符。
    #[test]
    fn versions_reject_what_breaks_the_manifest() {
        assert!(check_version("0.1.0").is_ok());
        assert!(check_version("").is_ok(), "空 = 用默认值");
        assert!(check_version("a\"b").is_err());
        assert!(check_version("a\\b").is_err());
    }

    /// TOML 基本字符串的转义：反斜杠和引号有转义，控制符换空格。
    #[test]
    fn toml_strings_are_escaped() {
        assert_eq!(toml_str("plain"), "plain");
        assert_eq!(toml_str("a\"b"), "a\\\"b");
        assert_eq!(toml_str("a\\b"), "a\\\\b");
        assert_eq!(toml_str("a\nb"), "a b");
    }

    /// 清单文本能被 dg-script 的解析器原样收下 —— id、脚本名都对得上。
    /// 解析器会核对「声明的脚本在包目录里存在」，所以先把 main.lua
    /// 落到临时目录，再把那份清单交给它。
    #[test]
    fn the_manifest_parses_with_the_engine() {
        let dir = std::env::temp_dir().join("dogedit-newscript-test-parse");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.lua"), "").unwrap();
        let text = package_manifest("hello", "你好", "1.2.3", "示例包 \"quoted\"");
        let m = dg_script::package::parse_manifest(&text, &dir).expect("清单应能解析");
        assert_eq!(m.id, "hello");
        assert_eq!(m.name, "你好");
        assert_eq!(m.version, "1.2.3");
        assert_eq!(m.scripts, vec!["main.lua".to_string()]);
        assert!(!m.is_on_demand(), "向导默认做开机装载的包，launch 只是注释");
        // 带引号的描述也没写坏清单。
        let text = package_manifest("hello", "", "", "示例包 \"quoted\"");
        let m = dg_script::package::parse_manifest(&text, &dir).expect("描述里的引号要转义");
        assert_eq!(m.description, "示例包 \"quoted\"");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 模板正文只落在桥接真有的那几个接口上；沙箱没有 os/io。
    #[test]
    fn templates_stay_on_the_bridge() {
        const KNOWN: [&str; 8] = [
            "App.alert(",
            "App.log(",
            "App.command(",
            "App.doc.line_count()",
            "App.doc.line(",
            "App.doc.replace(",
            "App.version(",
            "print(",
        ];
        for entry in [Entry::AutoRun, Entry::MenuCmd] {
            for tpl in 0..TEMPLATES.len() {
                let what = format!("入口 {entry:?} 模板 {tpl}");
                let body = lua_body(tpl, "demo", entry);
                assert!(
                    KNOWN.iter().any(|k| body.contains(k)),
                    "{what} 没用任何已知接口"
                );
                // 每个 App.xxx 引用的名字都必须是白名单里的前缀。
                for part in body.split("App.").skip(1) {
                    let name: String = part
                        .chars()
                        .take_while(|c| c.is_ascii_alphabetic() || *c == '_')
                        .collect();
                    if name.is_empty() {
                        continue;
                    }
                    let wanted = format!("App.{name}");
                    assert!(
                        KNOWN.iter().any(|k| k.starts_with(&wanted)),
                        "{what} 用了未知的 App.{name}"
                    );
                }
                assert!(!body.contains("os."), "{what} 用了 os");
                assert!(!body.contains("io."), "{what} 用了 io");
                // 每份生成文件都带自己的名字进注释，创建出来的文件读得懂。
                assert!(body.contains("demo.lua"), "{what} 注释里没有文件名");
                // 菜单命令入口下，任何模板的正文都挂在 App.command 上。
                if entry == Entry::MenuCmd {
                    assert!(
                        body.contains("App.command(\"demo\""),
                        "{what} 没包进命令注册"
                    );
                }
            }
        }
    }

    /// 组计划（散装）：落到指定目录，文件一份，装载名 = 文件名主干。
    #[test]
    fn loose_plans_write_one_file() {
        let dir = std::env::temp_dir().join("dogedit-newscript-test");
        let plan = build_plan_at(
            Kind::Loose,
            Entry::AutoRun,
            0,
            "demo",
            "",
            "",
            "",
            &dir,
            std::path::Path::new(""),
        )
        .expect("demo 还不存在");
        assert_eq!(plan.pkg_id, "demo");
        assert_eq!(plan.files.len(), 1);
        assert_eq!(plan.open, dir.join("demo.lua"));
        assert_eq!(plan.files[0].0, dir.join("demo.lua"));
        assert!(plan.files[0].1.contains("demo 已装载"));
    }

    /// 组计划（包）：main.lua 和清单各一份，装载名 = 清单 id，
    /// 显示名缺省用 id，版本空了用默认值。清单交给解析器验之前先把
    /// 计划里的文件按 `create_script` 的写法落到盘上 —— 解析器会核对
    /// 「声明的脚本在包目录里存在」。
    #[test]
    fn package_plans_write_manifest_and_main() {
        let root = std::env::temp_dir().join("dogedit-newscript-test-pkg");
        let plan = build_plan_at(
            Kind::Package,
            Entry::AutoRun,
            2,
            "demo",
            "",
            "",
            "一个示例",
            std::path::Path::new(""),
            &root,
        )
        .expect("demo 还不存在");
        assert_eq!(plan.pkg_id, "demo");
        assert_eq!(plan.files.len(), 2);
        assert_eq!(plan.open, root.join("demo").join("main.lua"));
        // create_script 的落盘顺序：目录建好，文件按序写。
        for (path, content) in &plan.files {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        let (manifest_path, manifest_text) = &plan.files[1];
        assert_eq!(manifest_path.file_name().unwrap(), "package.toml");
        let m = dg_script::package::parse_manifest(manifest_text, &root.join("demo"))
            .expect("清单应能解析");
        assert_eq!(m.name, "demo", "显示名缺省用 id");
        assert_eq!(m.version, "0.1.0", "版本空了用默认值");
        assert_eq!(m.description, "一个示例");
        let _ = std::fs::remove_dir_all(root);
    }

    /// 撞文件挡在计划这一步：已存在的 .lua / 包目录都建不了。
    #[test]
    fn collisions_are_caught_before_any_write() {
        let dir = std::env::temp_dir().join("dogedit-newscript-test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("taken.lua"), "").unwrap();
        assert!(build_plan_at(Kind::Loose, Entry::AutoRun, 0, "taken", "", "", "", &dir, &dir).is_err());
        let pkg = dir.join("takenpkg");
        std::fs::create_dir_all(&pkg).unwrap();
        assert!(build_plan_at(Kind::Package, Entry::AutoRun, 0, "takenpkg", "", "", "", &dir, &dir).is_err());
        let _ = std::fs::remove_file(dir.join("taken.lua"));
        let _ = std::fs::remove_dir_all(pkg);
    }

    /// 确认页的摘要把类型、位置、入口、文件都说到 —— 用户确认的就是
    /// 将要发生的事；入口行必须把「有没有菜单行」说死。
    #[test]
    fn the_summary_names_the_type_the_place_and_the_files() {
        let mut st = wizard(Kind::Loose, 1, "demo");
        st.entry = Entry::MenuCmd;
        let text = summary_text(&st);
        assert!(text.contains("散装脚本"), "{text}");
        assert!(text.contains("demo.lua"), "{text}");
        assert!(text.contains("提示框"), "{text}");
        assert!(text.contains("Lua: demo"), "{text}");
        assert!(text.contains("扩展 菜单底部"), "{text}");
        let st = wizard(Kind::Loose, 1, "demo");
        let text = summary_text(&st);
        assert!(text.contains("不出现在菜单"), "{text}");
        assert!(!text.contains("Lua: demo"), "{text}");
        let st = wizard(Kind::Package, 0, "demo");
        let text = summary_text(&st);
        assert!(text.contains("package.toml"), "{text}");
        assert!(text.contains("main.lua"), "{text}");
    }

    /// 菜单命令入口的生成物：正文包进 App.command，状态栏顺带报菜单去向。
    #[test]
    fn menucmd_plans_wrap_the_body_in_a_command() {
        let dir = std::env::temp_dir().join("dogedit-newscript-test-menucmd");
        let plan = build_plan_at(
            Kind::Loose,
            Entry::MenuCmd,
            2,
            "trimdemo",
            "",
            "",
            "",
            &dir,
            std::path::Path::new(""),
        )
        .expect("trimdemo 还不存在");
        let body = &plan.files[0].1;
        assert!(
            body.contains("App.command(\"trimdemo\""),
            "没包进命令注册：{body}"
        );
        assert!(body.contains("App.doc.replace("), "模板正文丢了：{body}");
        assert!(
            plan.message.contains("扩展 菜单底部"),
            "{}",
            plan.message
        );
        // 装载即运行的同款计划不带这些。
        let plan = build_plan_at(
            Kind::Loose,
            Entry::AutoRun,
            2,
            "trimdemo",
            "",
            "",
            "",
            &dir,
            std::path::Path::new(""),
        )
        .expect("trimdemo 还不存在");
        assert!(
            !plan.files[0].1.contains("App.command("),
            "装载即运行不该有命令注册"
        );
        assert!(!plan.message.contains("扩展 菜单底部"));
    }

    /// 保存位置：每个类型两条，都是引擎启动扫描的根；工作目录两条
    /// 解析到 cwd 下的同名目录，程序旁两条照旧走原来的助手。
    #[test]
    fn spots_follow_the_engine_roots() {
        assert_eq!(
            spot_choices(Kind::Loose),
            &[Spot::ExeScripts, Spot::WorkScripts][..]
        );
        assert_eq!(
            spot_choices(Kind::Package),
            &[Spot::UserPackages, Spot::DevPackages][..]
        );
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(spot_dir(Spot::WorkScripts).unwrap(), cwd.join("scripts"));
        assert_eq!(spot_dir(Spot::DevPackages).unwrap(), cwd.join("packages"));
        assert_eq!(spot_dir(Spot::ExeScripts), luascript::scripts_dir());
        assert_eq!(spot_dir(Spot::UserPackages), user_packages_root());
    }

    /// 选了工作目录的保存位置：确认页位置行带「（工作目录）」，预览
    /// 落在 cwd 的 scripts 下；程序旁 / 用户目录两条不加注。
    #[test]
    fn workdir_spots_change_the_summary_and_the_preview() {
        let mut st = wizard(Kind::Loose, 0, "demo");
        st.spot = Spot::WorkScripts;
        let text = summary_text(&st);
        assert!(text.contains("（工作目录）"), "{text}");
        assert!(text.contains("scripts"), "{text}");
        let files = preview_files(&st);
        assert_eq!(
            files,
            vec![std::env::current_dir()
                .unwrap()
                .join("scripts")
                .join("demo.lua")]
        );
        let mut st = wizard(Kind::Package, 0, "demo");
        st.spot = Spot::DevPackages;
        let text = summary_text(&st);
        assert!(text.contains("（工作目录）"), "{text}");
        let st = wizard(Kind::Loose, 0, "demo");
        assert!(!summary_text(&st).contains("（工作目录）"));
    }

    /// 每份生成文件都在真引擎里跑一遍：向导交付的是「能用」的脚本，不是
    /// 「看起来像」的脚本。桥给一个三行的小文档；行处理要真改到第一行，
    /// 菜单命令要真注册得上、调得动 —— 包进命令的行处理也照样改文档。
    #[test]
    fn every_template_loads_and_runs_in_the_engine() {
        use dg_script::Pos;
        struct Doc {
            lines: Vec<String>,
        }
        impl dg_script::Bridge for Doc {
            fn alert(&mut self, _msg: &str) {}
            fn log(&mut self, _msg: &str) {}
            fn doc_name(&self) -> String {
                "demo.txt".to_string()
            }
            fn doc_path(&self) -> Option<String> {
                None
            }
            fn line_count(&self) -> usize {
                self.lines.len()
            }
            fn line_text(&self, line: usize) -> Option<String> {
                self.lines.get(line - 1).cloned()
            }
            fn doc_text(&self) -> String {
                self.lines.join("\n")
            }
            fn span_text(&self, _s: Pos, _e: Pos) -> Option<String> {
                None
            }
            fn insert_at(&mut self, _p: Pos, _text: &str) -> bool {
                false
            }
            fn replace_span(&mut self, s: Pos, e: Pos, text: &str) -> bool {
                let ((sl, sc), (el, ec)) = (s, e);
                if sl != el {
                    return false;
                }
                let Some(t) = self.lines.get_mut(sl.wrapping_sub(1)) else {
                    return false;
                };
                let start = sc.saturating_sub(1).min(t.len());
                let end = ec.saturating_sub(1).min(t.len());
                if start > end {
                    return false;
                }
                t.replace_range(start..end, text);
                true
            }
            fn sel_count(&self) -> usize {
                0
            }
            fn sel_range(&self, _i: usize) -> Option<(Pos, Pos)> {
                None
            }
            fn sel_text(&self) -> String {
                String::new()
            }
            fn sel_set(&mut self, _s: Pos, _e: Pos) {}
        }
        let new_doc = || Doc {
            lines: vec!["  pad".to_string(), "x".to_string(), "y".to_string()],
        };
        for entry in [Entry::AutoRun, Entry::MenuCmd] {
            for tpl in 0..TEMPLATES.len() {
                let what = format!("入口 {entry:?} 模板 {tpl}");
                let host = dg_script::ScriptHost::new()
                    .unwrap_or_else(|e| panic!("{what}：引擎起不来 {e}"));
                let body = lua_body(tpl, "demo", entry);
                host.load(&format!("demo{entry:?}{tpl}.lua"), &body, &mut new_doc())
                    .unwrap_or_else(|e| panic!("{what} 装不上：{e}"));
                match entry {
                    Entry::AutoRun => {
                        if tpl == 2 {
                            // 行处理示例：第一行的行首空白该被去掉。
                            let mut doc = new_doc();
                            host.run(&format!("demo-run{tpl}.lua"), &body, &mut doc)
                                .unwrap_or_else(|e| panic!("{what} 跑不动：{e}"));
                            assert_eq!(doc.lines[0], "pad", "{what} 没改到第一行");
                        }
                    }
                    Entry::MenuCmd => {
                        // 注册的命令查得到、调得动；包进命令的行处理照样改文档。
                        let names = host.command_names();
                        assert!(
                            names.iter().any(|n| n == "demo"),
                            "{what} 没注册出命令 demo（注册了 {names:?}）"
                        );
                        let mut doc = new_doc();
                        assert!(
                            host.call_command("demo", &mut doc),
                            "{what} 注册的命令调不动"
                        );
                        if tpl == 2 {
                            assert_eq!(doc.lines[0], "pad", "{what} 包进命令后没改到第一行");
                        }
                    }
                }
            }
        }
    }

    /// `#[repr(C, align(4))]` 的模板头要和 DLGTEMPLATE 对得上。
    #[test]
    fn the_template_is_dword_aligned() {
        assert_eq!(std::mem::align_of::<Template>(), 4);
        assert_eq!(
            std::mem::size_of::<Template>(),
            std::mem::size_of::<DLGTEMPLATE>() + 6
        );
    }
}
