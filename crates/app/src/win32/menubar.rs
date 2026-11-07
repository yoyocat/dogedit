//! The main menu bar, row for row. Generated from a walk of the **running** menu with
//! `GetMenu(hwnd)`. The bar has 437 rows: 10 top-level popups and 317 leaves. Three families of
//! live rows are deliberately absent because they are state, not definition: the recent-file list
//! (ids 35000+), the installed syntax modes (2700+) and the plugin list (40000+). They are this
//! implementation's dynamic seams. A popup left with nothing but such rows (`插件(&P)`) was dropped
//! with them rather than emitted as a childless parent. `OWNERDRAW` — which the bar puts on every
//! top-level popup under 查看/工具/ 窗口/帮助 and on most of their sub-popups — is not carried in this
//! table. This implementation now applies `MFT_OWNERDRAW` to every row in `menu.rs` instead, so the
//! table stays a pure description of ids, captions and structure.

use crate::app::{Editor, WrapMode};
use crate::cmdids;
use crate::keys::Command;
use dg_core::Eol;
use dg_core::folding::FoldingMethod;
use dg_ui::menu::MenuItem;

/// Row kinds in `ROWS`.
const LEAF: u8 = 0;
const POPUP: u8 = 1;
const SEP: u8 = 2;

/// One row: `(depth, kind, id, caption)`. `depth` 0 is the bar itself, so a
/// popup's children are the following rows at `depth + 1`. Captions are the
/// localised text as the live menu reports it, mnemonics included.
type Row = (u8, u8, u32, &'static str);

const ROWS: &[Row] = &[
    (0, POPUP, 0, "文件(&F)"),
    (1, POPUP, 0, "新建(&N)"),
    (2, LEAF, 57600, "普通文本(&N)"),
    (2, LEAF, 1233, "二进制(&B)"),
    (2, LEAF, 1230, "配置模板(&C)"),
    (2, LEAF, 1238, "其它文件类型(&O)..."),
    (1, LEAF, 57601, "打开(&O)..."),
    (1, LEAF, 1248, "打开文件夹内文件(&D)..."),
    (1, SEP, 0, ""),
    (1, LEAF, 57603, "保存(&S)"),
    (1, LEAF, 1234, "保存全部(&S)"),
    (1, LEAF, 57604, "另存为(&A)..."),
    (1, LEAF, 36010, "备份与历史(&K)..."),
    (1, SEP, 0, ""),
    (1, LEAF, 1237, "重新读取(&R)"),
    (1, LEAF, 1658, "指定编码加载(&E)"),
    (1, SEP, 0, ""),
    (1, LEAF, 2012, "新建实例(&I)"),
    (1, SEP, 0, ""),
    (1, LEAF, 57609, "打印预览(&P)"),
    (1, LEAF, 57607, "打印(&P)..."),
    (1, SEP, 0, ""),
    (1, POPUP, 0, "会话"),
    (2, LEAF, 1241, "保存到新会话..."),
    (2, LEAF, 1243, "保存到当前会话"),
    (2, LEAF, 1242, "清除会话菜单"),
    (1, POPUP, 0, "最近的文件(&R)"),
    (2, LEAF, 1236, "清除历史记录(&H)"),
    // 清除历史记录下面没有静态分隔线：那根线属于动态行块，由
    // rebuild_recent_menu 在列表非空时和行一起追加（会话弹出菜单同款），
    // 否则静态一根、动态一根，菜单里会出现两根线。
    (1, LEAF, 1240, "恢复最后关闭的文件(&C)"),
    (1, SEP, 0, ""),
    (1, LEAF, 57665, "退出(&X)"),
    (0, POPUP, 0, "编辑(&E)"),
    (1, LEAF, 57643, "撤消(&U)"),
    (1, LEAF, 57644, "重做(&R)"),
    (1, SEP, 0, ""),
    (1, LEAF, 57635, "剪切(&T)"),
    (1, LEAF, 57634, "复制(&C)"),
    (1, LEAF, 92, "复制为RTF(&R)"),
    (1, LEAF, 57637, "粘贴(&P)"),
    (1, LEAF, 1338, "列模式(&C)"),
    (1, SEP, 0, ""),
    (1, POPUP, 0, "剪贴板(&C)"),
    (2, LEAF, 87, "剪切并追加(&A)"),
    (2, LEAF, 88, "复制并追加(&C)"),
    (2, SEP, 0, ""),
    (2, LEAF, 90, "剪切标记行(&M)"),
    (2, LEAF, 89, "复制标记行(&L)"),
    (2, SEP, 0, ""),
    (2, LEAF, 91, "列模式粘贴(&P)"),
    (2, LEAF, 94, "行尾列模式粘贴(&T)"),
    (2, LEAF, 61, "缩进粘贴(保持缩进格式)(&I)"),
    (2, LEAF, 1695, "从剪贴板历史中粘贴(&H)"),
    (2, LEAF, 112, "粘贴\\0字符"),
    (2, SEP, 0, ""),
    (2, LEAF, 1429, "清空剪贴板(&E)"),
    (1, POPUP, 0, "删除(&D)"),
    (2, LEAF, 26, "删除(&D)"),
    (2, LEAF, 27, "删除下一个字符(&N)"),
    (2, SEP, 0, ""),
    (2, LEAF, 85, "删除单词(&W)"),
    (2, LEAF, 28, "向左删除单词(&P)"),
    (2, LEAF, 29, "向右删除单词(&N)"),
    (2, LEAF, 30, "删除整行(&L)"),
    (2, LEAF, 79, "删除到行头(&H)"),
    (2, LEAF, 80, "删除到行尾(&E)"),
    (2, SEP, 0, ""),
    (2, LEAF, 1428, "删除空白行"),
    (2, LEAF, 1452, "删除重复行(从上往下)"),
    (2, LEAF, 1409, "删除重复行(从下往上)"),
    (2, SEP, 0, ""),
    (2, LEAF, 63, "删除行头空白(&L)"),
    (2, LEAF, 64, "删除行尾空白(T)"),
    (1, POPUP, 0, "选择(&S)"),
    (2, LEAF, 57642, "全选(&A)"),
    (2, LEAF, 46, "选择单词(&W)"),
    (2, LEAF, 86, "选择整行(&L)"),
    (2, LEAF, 67, "选择到匹配括号(&P)"),
    (2, LEAF, 77, "选择到匹配括号(包含括号)"),
    (2, SEP, 0, ""),
    (2, LEAF, 55, "选择到上一行(&U)"),
    (2, LEAF, 56, "选择到下一行(&D)"),
    (2, SEP, 0, ""),
    (2, LEAF, 106, "打散选区到每一行"),
    (1, POPUP, 0, "对齐"),
    (2, LEAF, 1480, "向左对齐(&L)"),
    (2, LEAF, 1482, "居中对齐(&C)"),
    (2, LEAF, 1481, "向右对齐(&R)"),
    (1, SEP, 0, ""),
    (1, POPUP, 0, "转换(&C)"),
    (2, LEAF, 102, "单词首字母大写(&C)"),
    (2, LEAF, 57, "转为大写(&U)"),
    (2, LEAF, 58, "转为小写(&L)"),
    (2, LEAF, 82, "反转大小写(&I)"),
    (2, SEP, 0, ""),
    (2, LEAF, 97, "转为全角(&F)"),
    (2, LEAF, 98, "转为半角(&H)"),
    (2, SEP, 0, ""),
    (2, LEAF, 69, "制表符转为空格(&T)"),
    (2, LEAF, 70, "空格转为制表符(&S)"),
    (2, LEAF, 83, "行头制表符转为空格(&L)"),
    (2, LEAF, 84, "行头空格转为制表符(&L)"),
    (1, POPUP, 0, "格式(&F)"),
    (2, LEAF, 47, "增加缩进(&I)"),
    (2, LEAF, 48, "减少缩进(&D)"),
    (2, LEAF, 51, "格式化缩进(&F)"),
    (2, SEP, 0, ""),
    (2, LEAF, 1413, "单行注释(&C)"),
    (2, LEAF, 1414, "取消单行注释(&U)"),
    (2, LEAF, 1415, "多行注释(&B)"),
    (2, LEAF, 1416, "取消多行注释(&B)"),
    (1, POPUP, 0, "时间和日期(&D)"),
    (2, LEAF, 1417, "日期(&D)"),
    (2, LEAF, 1418, "时间(&T)"),
    (2, LEAF, 1419, "自定义日期/时间"),
    (1, POPUP, 0, "更多编辑(&O)..."),
    (2, LEAF, 72, "合并行(&J)"),
    (2, LEAF, 54, "重复行(&L)"),
    (2, LEAF, 108, "重复选区(&S)"),
    (2, LEAF, 68, "复制行"),
    (2, LEAF, 109, "恢复最近的选区"),
    (2, LEAF, 81, "交换字符(&T)"),
    (2, LEAF, 1487, "激活选区锚点(&A)"),
    (2, LEAF, 36, "切换覆写模式"),
    (1, SEP, 0, ""),
    (1, LEAF, 1483, "使用虚拟空格(&V)"),
    (1, LEAF, 1671, "使用软空格(&U)"),
    (1, SEP, 0, ""),
    (1, LEAF, 1423, "部分编辑(&P)"),
    (1, LEAF, 1412, "HEX模式(&M)"),
    (0, POPUP, 0, "查看(&V)"),
    (1, POPUP, 0, "字体(&F)"),
    (2, LEAF, 1430, "默认字体(Consolas,10)"),
    (2, SEP, 0, ""),
    (2, LEAF, 1431, "自定义字体1"),
    (2, LEAF, 1432, "自定义字体2"),
    (2, LEAF, 1433, "自定义字体3"),
    (2, LEAF, 1434, "自定义字体4"),
    (2, LEAF, 1435, "自定义字体5"),
    (2, SEP, 0, ""),
    (2, LEAF, 1424, "加大字号(&L)"),
    (2, LEAF, 1425, "减小字号(&S)"),
    (2, LEAF, 1469, "原始字号(&O)"),
    (2, SEP, 0, ""),
    (2, LEAF, 1321, "设置字体(&F)"),
    (1, POPUP, 0, "工具栏(&T)"),
    (2, LEAF, 1324, "标签栏(&B)"),
    (2, LEAF, 59392, "工具条(&T)"),
    (2, LEAF, 59393, "状态栏(&S)"),
    (2, LEAF, 1317, "隐藏主菜单"),
    (2, SEP, 0, ""),
    (2, LEAF, 1314, "模式工具条(&M)"),
    (2, LEAF, 1712, "外部工具条(&E)"),
    (1, POPUP, 0, "停靠窗格(&D)"),
    (2, LEAF, 1312, "大纲视图(&O)"),
    (2, LEAF, 1292, "快捷目录(&D)"),
    (2, LEAF, 1291, "文件列表(&O)"),
    (2, LEAF, 1329, "代码片段(&S)"),
    (2, LEAF, 1322, "命令窗口(&C)"),
    (2, LEAF, 1289, "书签管理器"),
    (2, LEAF, 1286, "收藏夹"),
    (2, LEAF, 1285, "目录树"),
    (2, LEAF, 1489, "Git 面板"),
    (2, SEP, 0, ""),
    (2, LEAF, 1325, "输出窗口(&O)"),
    (2, LEAF, 1315, "下一个结果"),
    (2, LEAF, 1316, "上一个结果"),
    (1, SEP, 0, ""),
    (1, LEAF, 1318, "自定义标记(&C)"),
    (1, LEAF, 1300, "行号(&L)"),
    (1, LEAF, 1287, "文档地图(&I)"),
    (1, LEAF, 1301, "标尺(&R)"),
    (1, SEP, 0, ""),
    (1, LEAF, 1326, "括弧高亮(&B)"),
    (1, LEAF, 1327, "高亮网址和邮件(&H)"),
    (1, LEAF, 1313, "选区高亮(&S)"),
    (1, LEAF, 1271, "颜色高亮(&O)"),
    (1, SEP, 0, ""),
    (1, LEAF, 1455, "显示函数提示"),
    (1, LEAF, 1456, "显示自动完成候选词(&S)"),
    (1, POPUP, 0, "颜文字"),
    (2, LEAF, 1334, "显示颜文字(&S)"),
    (2, LEAF, 1486, "插入颜文字(&I)..."),
    (1, POPUP, 0, "符号显示(&S)"),
    (2, LEAF, 1305, "当前行"),
    (2, LEAF, 1307, "缩进线"),
    (2, LEAF, 1311, "边界线"),
    (2, LEAF, 1337, "列标记"),
    (2, LEAF, 1672, "图标区域"),
    (2, SEP, 0, ""),
    (2, LEAF, 1302, "空白字符"),
    (2, LEAF, 1303, "换行符"),
    (2, LEAF, 1304, "文档结束符"),
    (2, LEAF, 1306, "控制字符"),
    (2, SEP, 0, ""),
    (2, LEAF, 1310, "选择时全部显示"),
    (2, LEAF, 1309, "折叠尾部字符"),
    (2, LEAF, 1673, "扩展当前行背景"),
    (2, LEAF, 1674, "同层背景色"),
    (2, LEAF, 1675, "行修改状态"),
    (2, LEAF, 1676, "显示书签"),
    (1, POPUP, 0, "代码折叠(&F)"),
    (2, LEAF, 1330, "取消折叠(&C)"),
    (2, SEP, 0, ""),
    (2, LEAF, 1331, "语法折叠(&S)"),
    (2, LEAF, 1332, "缩进折叠(&I)"),
    (2, SEP, 0, ""),
    (2, LEAF, 62, "折叠当前级别"),
    (2, LEAF, 52, "展开当前级别"),
    (2, LEAF, 1339, "折叠全部(&F)"),
    (2, LEAF, 1340, "展开全部(&U)"),
    (2, SEP, 0, ""),
    (2, LEAF, 1622, "折叠级别:2"),
    (2, LEAF, 1623, "折叠级别:3"),
    (2, LEAF, 1624, "折叠级别:4"),
    (2, LEAF, 1625, "折叠级别:5"),
    (2, LEAF, 1626, "折叠级别:6"),
    (2, LEAF, 1627, "折叠级别:7"),
    (2, LEAF, 1628, "折叠级别:8"),
    (2, LEAF, 1629, "折叠级别:9"),
    (1, SEP, 0, ""),
    (1, LEAF, 1290, "Web预览(&P)"),
    (1, LEAF, 1288, "外部浏览器预览"),
    (1, SEP, 0, ""),
    (1, LEAF, 1323, "设置行高(&L)"),
    (1, LEAF, 1328, "全屏显示(&F)"),
    (1, SEP, 0, ""),
    (1, LEAF, 1711, "深色主题(&D)"),
    (0, POPUP, 0, "查找(&S)"),
    (1, POPUP, 0, "跳转"),
    (2, LEAF, 1760, "跳转到文件(&A)..."),
    (2, LEAF, 1762, "跳转到工程符号(&P)..."),
    (2, LEAF, 1426, "跳转到行(&G)"),
    (2, LEAF, 1663, "跳转到下一个配对字符"),
    (1, LEAF, 57636, "查找(&F)"),
    (1, LEAF, 1401, "查找下一个(&N)"),
    (1, LEAF, 1402, "查找上一个(&P)"),
    (1, LEAF, 1406, "查找下一个单词(&W)"),
    (1, LEAF, 1407, "查找上一个单词(&O)"),
    (1, SEP, 0, ""),
    (1, LEAF, 1403, "查找全部(&A)"),
    (1, LEAF, 1408, "在文件中查找和替换(&I)..."),
    (1, LEAF, 1470, "迷你查找..."),
    (1, POPUP, 0, "快速选择(&Q)"),
    (2, LEAF, 1410, "快速选择(&Q)"),
    (2, LEAF, 1422, "选择所有匹配(&O)"),
    (2, SEP, 0, ""),
    (2, LEAF, 1484, "大小写匹配"),
    (2, LEAF, 1485, "智能匹配单词边界"),
    (1, SEP, 0, ""),
    (1, LEAF, 57641, "替换(&R)"),
    (1, LEAF, 1420, "批量替换(&B)..."),
    (1, SEP, 0, ""),
    (1, POPUP, 0, "书签(&M)"),
    (2, LEAF, 1276, "切换书签(&T)"),
    (2, LEAF, 1279, "下一个书签(&N)"),
    (2, LEAF, 1278, "上一个书签(&P)"),
    (2, LEAF, 1277, "删除全部书签(&D)"),
    (2, SEP, 0, ""),
    (2, LEAF, 90, "剪切标记行(&M)"),
    (2, LEAF, 89, "复制标记行(&L)"),
    (2, SEP, 0, ""),
    (2, LEAF, 1289, "书签管理器"),
    (2, LEAF, 1676, "显示书签"),
    (1, POPUP, 0, "图标提示"),
    (2, LEAF, 1280, "下一个提示(&N)"),
    (2, LEAF, 1281, "上一个提示(&P)"),
    (2, LEAF, 1282, "清除所有提示(&C)"),
    (1, POPUP, 0, "光标位置"),
    (2, LEAF, 105, "上一个编辑位置"),
    (2, LEAF, 104, "下一个编辑位置"),
    (2, SEP, 0, ""),
    (2, LEAF, 120, "上一个光标位置"),
    (2, LEAF, 119, "下一个光标位置"),
    (1, SEP, 0, ""),
    (1, LEAF, 60, "括号匹配(&M)"),
    (0, POPUP, 0, "文档(&D)"),
    (1, POPUP, 0, "自动换行(&W)"),
    (2, LEAF, 1294, "取消换行(&C)"),
    (2, SEP, 0, ""),
    (2, LEAF, 1297, "智能换行(&S)"),
    (2, LEAF, 1296, "窗口边界处换行(&W)"),
    (2, LEAF, 1295, "指定列换行(&F)"),
    (2, LEAF, 1293, "指定列换行并扩展制表符(&F)"),
    (2, LEAF, 1283, "指定屏幕宽度换行(&W)"),
    (2, SEP, 0, ""),
    (2, LEAF, 1308, "换行时缩进子行"),
    (2, LEAF, 1677, "列选之后恢复自动换行"),
    (1, POPUP, 0, "换行符格式(&N)"),
    (2, LEAF, 1449, "PC (CR+LF)"),
    (2, LEAF, 1450, "Unix (LF)"),
    (2, LEAF, 1451, "Mac (CR)"),
    (1, LEAF, 1662, "切换文件类型(&T)..."),
    (1, SEP, 0, ""),
    (1, LEAF, 1665, "禁止编辑(&D)"),
    (1, LEAF, 1661, "阅览模式(&B)"),
    (1, LEAF, 1664, "写作模式(&F)"),
    (1, LEAF, 1666, "竖排书写模式(&V)"),
    (1, SEP, 0, ""),
    (1, LEAF, 1659, "转换编码(&C)"),
    (1, LEAF, 1669, "批量转码・换行符(&M)..."),
    (1, SEP, 0, ""),
    (1, LEAF, 1654, "自动缩进(&I)"),
    (1, LEAF, 1657, "自动配对(&P)"),
    (1, LEAF, 1427, "自动完成(&C)"),
    (1, LEAF, 1488, "自动纠正关键字大小写(&K)"),
    (1, SEP, 0, ""),
    (1, LEAF, 1648, "重命名"),
    (1, LEAF, 1650, "复制路径(&P)"),
    (1, LEAF, 1649, "复制文件名(&N)"),
    (1, LEAF, 1655, "在文件管理器中打开(&O)"),
    (1, SEP, 0, ""),
    (1, LEAF, 1651, "监视剪贴板(&M)"),
    (1, POPUP, 0, "监视该文件变更"),
    (2, LEAF, 1751, "使用全局监视设置"),
    (2, LEAF, 1752, "提示变更"),
    (2, LEAF, 1753, "自动读取"),
    (2, LEAF, 1754, "自动读取并滚动到最后"),
    (2, SEP, 0, ""),
    (2, LEAF, 1755, "使用定时自动读取"),
    (1, LEAF, 1668, "在子行处换行(&L)"),
    (1, LEAF, 1667, "字符统计(&S)"),
    (1, SEP, 0, ""),
    (1, POPUP, 0, "高级(&A)"),
    (2, LEAF, 1653, "重新加载着色(&U)"),
    (2, LEAF, 1647, "重新加载模式(&M)"),
    (2, LEAF, 1660, "查看Scope(&V)"),
    // 本实现扩展（id 段 36000+）：解析器插件包重扫。
    (2, LEAF, 36007, "重新扫描解析器(&R)"),
    (0, POPUP, 0, "工程(&P)"),
    (1, LEAF, 1770, "打开工程(&O)..."),
    (1, LEAF, 1771, "另存为工程(&S)..."),
    (1, LEAF, 1772, "添加文件夹(&A)..."),
    (1, LEAF, 1773, "清空工程(&R)"),
    (1, LEAF, 1774, "刷新工程(&F)"),
    (1, LEAF, 1775, "关闭工程(&C)"),
    (0, POPUP, 0, "工具(&T)"),
    (1, POPUP, 0, "设置(&P)"),
    (2, LEAF, 1336, "常规(&G)"),
    (2, SEP, 0, ""),
    (2, LEAF, 1298, "主题(&T)"),
    (2, LEAF, 1321, "设置字体(&F)"),
    (2, LEAF, 1460, "快捷键(&S)"),
    (2, LEAF, 1459, "语法着色(&H)"),
    // 本实现扩展（id 段 36000+）：在线解析器列表的管理框。
    (2, LEAF, 36011, "解析器下载(&D)..."),
    (2, LEAF, 1580, "外部工具(&E)"),
    (1, SEP, 0, ""),
    (1, POPUP, 0, "拼写检查(&C)"),
    (2, LEAF, 1684, "即时拼写检查(&V)"),
    (2, LEAF, 1683, "以对话框的形式检查(&P)"),
    (2, LEAF, 1692, "设置"),
    (1, POPUP, 0, "排序(&S)"),
    (2, LEAF, 1681, "升序排序"),
    (2, LEAF, 1682, "降序排序"),
    (2, SEP, 0, ""),
    (2, LEAF, 1652, "高级排序"),
    (1, LEAF, 1615, "文件比较(&D)"),
    (1, LEAF, 1616, "文件比较(外部)"),
    (1, LEAF, 1612, "序列(&Q)"),
    (1, LEAF, 1614, "选区求和(&U)"),
    (1, LEAF, 1613, "取色器"),
    (1, POPUP, 0, "&Base64"),
    (2, LEAF, 1617, "以UTF8进行Base64编码"),
    (2, LEAF, 1618, "以UTF8进行Base64解码"),
    (2, SEP, 0, ""),
    (2, LEAF, 1619, "指定编码进行Base64编码"),
    (2, LEAF, 1620, "指定编码进行Base64解码"),
    (1, POPUP, 0, "加密/解密(&N)"),
    (2, LEAF, 1453, "加密(&N)"),
    (2, LEAF, 1454, "解密(&D)"),
    (1, SEP, 0, ""),
    (1, LEAF, 1580, "外部工具(&E)"),
    (0, POPUP, 0, "扩展(&A)"),
    (1, POPUP, 0, "宏"),
    (2, LEAF, 1570, "录制/停止(&R)"),
    (2, LEAF, 1578, "暂停(&A)"),
    (2, LEAF, 1571, "回放(&P)"),
    (2, LEAF, 1573, "重复播放(&E)"),
    (2, SEP, 0, ""),
    (2, LEAF, 1572, "保存宏为脚本(&S)"),
    (2, LEAF, 1576, "读取(&L)"),
    (2, LEAF, 1577, "宏编辑(&V)"),
    (1, POPUP, 0, "事件(&E)"),
    (2, LEAF, 1564, "激活事件脚本(&E)"),
    (2, LEAF, 1565, "重新加载事件脚本(&R)"),
    (1, POPUP, 0, "模式(&M)"),
    (2, LEAF, 1314, "模式工具条(&M)"),
    (1, SEP, 0, ""),
    // 本实现扩展（id 段 36000+）：新建脚本向导（散装脚本或脚本包，选模板、
    // 填名字，落盘即装载）。
    (1, LEAF, 36013, "新建脚本(&N)..."),
    // 本实现扩展（id 段 36000+）：脚本包（启用/停用、看它为什么没在跑）。
    // 和新建脚本放一块 —— 两个都是脚本的事，留在设置里反而要跨菜单找。
    (1, LEAF, 36012, "脚本包(&K)..."),
    (1, LEAF, 1574, "刷新脚本菜单(&M)"),
    // 扫描与脚本命令的动态行在运行时接在 刷新脚本菜单 之后(34000+ / 35100+)。
    (0, POPUP, 0, "窗口(&I)"),
    (1, LEAF, 1222, "显示/隐藏所有面板(&H)"),
    (1, SEP, 0, ""),
    (1, LEAF, 1200, "下一个窗口(&N)"),
    (1, LEAF, 1201, "上一个窗口(&P)"),
    (1, LEAF, 1212, "下一个窗口(前后顺序)"),
    (1, LEAF, 1213, "上一个窗口(前后顺序)"),
    (1, SEP, 0, ""),
    (1, LEAF, 1206, "关闭(&C)"),
    (1, LEAF, 1203, "关闭其它窗口(&C)"),
    (1, LEAF, 1202, "关闭全部窗口(&A)"),
    (1, SEP, 0, ""),
    (1, LEAF, 1204, "垂直分割(&V)"),
    (1, LEAF, 1205, "水平分割(&H)"),
    (1, LEAF, 1207, "克隆视图"),
    (1, SEP, 0, ""),
    (1, LEAF, 1209, "移动到垂直分组(&V)"),
    (1, LEAF, 1211, "移动到水平分组(&H)"),
    (1, LEAF, 1210, "合并分组"),
    (1, SEP, 0, ""),
    (1, LEAF, 1208, "同步滚动(&Y)"),
    (1, LEAF, 1223, "切换焦点(&S)"),
    (1, LEAF, 1221, "最近的窗口(&L)"),
    (1, SEP, 0, ""),
    (1, LEAF, 1214, "窗口置顶(&A)"),
    (1, POPUP, 0, "透明度(&T)"),
    (2, LEAF, 1220, "不透明(&O)"),
    (2, SEP, 0, ""),
    (2, LEAF, 1215, "80%"),
    (2, LEAF, 1216, "60%"),
    (2, LEAF, 1217, "40%"),
    (2, LEAF, 1218, "20%"),
    (2, LEAF, 1219, "10%"),
    (2, SEP, 0, ""),
    (2, LEAF, 1226, "自定义..."),
    (0, POPUP, 0, "帮助(&H)"),
    (1, LEAF, 2010, "在线帮助(&H)"),
    (1, SEP, 0, ""),
    (1, LEAF, 2009, "关联帮助(&C)"),
    (1, LEAF, 2005, "快捷键列表(&K)"),
    (1, SEP, 0, ""),
    (1, LEAF, 2002, "打开安装目录"),
    (1, LEAF, 2004, "报告错误(&R)"),
    (1, LEAF, 2014, "清除注册信息(&I)"),
    (1, LEAF, 2003, "检查更新(&C)"),
    (1, SEP, 0, ""),
    (1, LEAF, 57664, "关于(&A) DogEdit"),
];

/// The caption a command id carries in the bar. The 工具条 options page lists
/// every palette command by this name
/// names its toolbar buttons.
pub(super) fn command_caption(id: u32) -> Option<&'static str> {
    ROWS
        .iter()
        .find(|&&(depth, kind, row, _)| depth > 0 && kind == LEAF && row == id)
        .map(|&(_, _, _, caption)| caption)
}

/// The bar position of the top-level popup whose caption starts with
/// `prefix` — the ordinal `GetSubMenu` takes — paired with its row in
/// `ROWS`, where the popup's own children begin. `None` when the bar has
/// no such popup.
///
/// The two numbers differ: `ROWS` is one flat table (文件's rows, then
/// 编辑's, …), so a row's position in it is not its position in the bar.
fn top_level(prefix: &str) -> Option<(usize, usize)> {
    let mut ordinal = 0;
    for (row, &(depth, kind, _, caption)) in ROWS.iter().enumerate() {
        if depth == 0 && kind == POPUP {
            if caption.starts_with(prefix) {
                return Some((ordinal, row));
            }
            ordinal += 1;
        }
    }
    None
}

/// The count of a popup's static rows: every depth-1 row from `start`
/// until the next top-level popup. Its nested popups sit between them at
/// depth 2, so they do not count.
fn static_row_count(start: usize) -> usize {
    let mut count = 0;
    for &(depth, _, _, _) in &ROWS[start..] {
        if depth == 0 {
            break;
        }
        if depth == 1 {
            count += 1;
        }
    }
    count
}

/// The 扩展(&A) row's index in the bar
/// `GetSubMenu(bar, 7)` for 刷新脚本菜单 (1574) and the
/// lazy script-menu seed. Computed from `ROWS` so the
/// table stays the single source of the bar's shape;
/// falls back to index 7.
pub(super) fn ext_menu_index() -> usize {
    top_level("扩展").map_or(7, |(pos, _)| pos)
}

/// The count of the 扩展 menu's static rows — its three popups, the
/// separator, 新建脚本 (36013), 脚本包 (36012) and 刷新脚本菜单 (1574) —
/// where the dynamic script rows (34000+) begin. `ROWS` is static, so it
/// is always 7.
pub(super) fn ext_menu_base_count() -> usize {
    top_level("扩展").map_or(7, |(_, row)| static_row_count(row + 1))
}

/// The 工具(&T) popup's index in the bar — where the
/// configured external tools' rows are appended, after
/// the static 外部工具... row. Computed from `ROWS` the
/// same way the 扩展 index is.
pub(super) fn tools_menu_index() -> usize {
    top_level("工具").map_or(6, |(pos, _)| pos)
}

/// The count of the 工具 menu's static rows — every depth-1 row before
/// the next top-level popup — where the dynamic tool rows (1601+)
/// begin.
pub(super) fn tools_menu_base_count() -> usize {
    top_level("工具").map_or(13, |(_, row)| static_row_count(row + 1))
}

/// The 文件(&F) popup's index in the bar — the top-level menu whose two
/// sub-popups (会话, 最近的文件) carry the dynamic rows. Computed from
/// `ROWS` the way the 扩展 and 工具 indices are; falls back to 0, its index
/// in the built bar.
pub(super) fn file_menu_index() -> usize {
    top_level("文件").map_or(0, |(pos, _)| pos)
}

/// The 窗口(&I) popup's index in the bar — the one whose split rows
/// (1204/1205/1207/1208-1211) grey and mark with the frame's split
/// state. Computed from `ROWS` like the other indices; falls back to 3, its
/// index in the built bar.
pub(super) fn window_menu_index() -> usize {
    top_level("窗口").map_or(3, |(pos, _)| pos)
}

/// The 文档(&D) popup's index in the bar — the one holding the 监视该文件
/// 变更 sub-popup. Computed from `ROWS` like the other indices; falls
/// back to 4, its index in the built bar.
pub(super) fn doc_menu_index() -> usize {
    top_level("文档").map_or(4, |(pos, _)| pos)
}

/// The two dynamic popups inside the 文件 menu, as positions among its
/// children: the 会话 popup, whose saved-session rows (ids 2801+) are
/// appended after its three static rows, and the 最近的文件 popup, whose
/// recent-file rows (ids 35001+) follow 清除历史记录 and its separator.
/// Computed from `ROWS` the way the 扩展 and 工具 indices are, so the table
/// stays the single source of the bar's shape; falls back to the table's own
/// positions (17, 18) if the captions ever move.
pub(super) fn file_menu_dynamic_popups() -> (usize, usize) {
    let mut session = None;
    let mut recent = None;
    let mut pos = 0usize;
    let mut i = match ROWS
        .iter()
        .position(|&(depth, kind, _, c)| depth == 0 && kind == POPUP && c.starts_with("文件"))
    {
        Some(i) => i + 1,
        None => return (17, 18),
    };
    while i < ROWS.len() && ROWS[i].0 > 0 {
        let (depth, kind, _, caption) = ROWS[i];
        if depth == 1 {
            if kind == POPUP {
                if caption == "会话" && session.is_none() {
                    session = Some(pos);
                }
                if caption.starts_with("最近的文件") && recent.is_none() {
                    recent = Some(pos);
                }
            }
            pos += 1;
        }
        i += 1;
    }
    (
        session.unwrap_or(17),
        recent.unwrap_or(18),
    )
}

/// The 监视该文件变更 popup's position among the 文档 menu's children:
/// the depth-1 popup whose first row is the 使用全局监视设置 row.
/// The mark rules and the one enable rule (定时自动读取只在两档自动
/// 读取下可选) are applied to this popup when it opens.
pub(super) fn monitor_popup_pos() -> usize {
    let mut pos = 0usize;
    let mut i = match ROWS
        .iter()
        .position(|&(depth, kind, _, c)| depth == 0 && kind == POPUP && c.starts_with("文档"))
    {
        Some(i) => i + 1,
        None => return usize::MAX,
    };
    while i < ROWS.len() && ROWS[i].0 > 0 {
        let (depth, kind, _, _) = ROWS[i];
        if depth == 1 {
            if kind == POPUP && ROWS.get(i + 1).map_or(false, |&r| r.0 == 2 && r.2 == 1751) {
                return pos;
            }
            pos += 1;
        }
        i += 1;
    }
    usize::MAX
}

/// Whether a 文件-menu row is greyed at this moment. The rest of the bar's
/// rows key off the command table alone: - 清除历史记录 (1236) and 清除会话菜单 (1242)
/// live only while their list has entries (the recent-file list and the
/// session list the frame keeps); - 保存到当前会话 (1243) needs a current session
/// name (the name the frame remembers, which must not be empty).
pub(super) fn row_enabled(
    id: u32,
    recent_count: usize,
    session_count: usize,
    current_session: Option<&str>,
) -> bool {
    match id {
        1236 => recent_count > 0,
        1242 => session_count > 0,
        1243 => current_session.is_some_and(|s| !s.is_empty()),
        _ => true,
    }
}

/// Whether a bar row carries a check mark at this moment. The row's own command
/// decides it, so the `ROWS` table stays the single source of the menu's shape: a row
/// is marked exactly when the document already sits in the state that row selects. The
/// wrap submenu shows one of its six modes, 代码折叠 one of its methods, the 换行符 rows the
/// document's EOL, and the `cm_view_*` rows their option bit.
pub(super) struct Chrome {
    pub toolbar: bool,
    /// 标签栏 (1324): the document tab strip's own switch, the sibling of
    /// `toolbar` one row down in the 视图 menu.
    pub tabbar: bool,
    pub statusbar: bool,
    pub modebar: bool,
    pub menu: bool,
    pub exttoolbar: bool,
    /// 颜色高亮 (1271) / 选区高亮 (1313) /
    /// 高亮网址和邮件 (1327): the frame-global
    /// highlight toggles, read by their menu rows.
    pub color_highlight: bool,
    pub sel_highlight: bool,
    pub url_highlight: bool,
    /// 自动配对 (1657) / 自动缩进 (1654): the
    /// editor's own latches, read by their menu rows.
    pub auto_pair: bool,
    pub auto_indent: bool,
    /// 激活事件脚本 (1564) / 监视剪贴板 (1651):
    /// the frame-global latches, read by their rows.
    pub enable_event: bool,
    pub watch_clipboard: bool,
    /// 显示颜文字 (1334): the emoji-rendering switch under
    /// 查看 → 颜文字.
    pub show_emoji: bool,
    /// 深色主题 (1711): the frame's dark/light mode. The row marks itself off
    /// this flag, not off the palette on screen: a hand-picked theme leaves the
    /// mode as it was.
    pub dark_theme: bool,
}

pub(super) fn check_of(
    id: u32,
    ed: &Editor,
    wrap: WrapMode,
    fullscreen: bool,
    chrome: &Chrome,
) -> bool {
    let Some(cmd) = cmdids::command_for_id(id) else {
        // 使用虚拟空格 (1483) 一类：没有命令表行、直接落在视图位上的
        // 裸开关行。位表自己记着菜单 id 到位的映射，勾选状态和开关
        // 写的是同一个位。
        if let Some(bit) = dg_render::ViewFlags::bit_for_menu_id(id) {
            return ed.view_flags.get(bit);
        }
        // 大小写匹配 (1484) / 智能匹配单词边界 (1485)：快速选择的
        // 两只勾，读的是文档自己的选区匹配开关。
        if id == 1484 {
            return ed.sel_match_case();
        }
        if id == 1485 {
            return ed.sel_match_word();
        }
        // 监视该文件变更 (1751-1755)：四档单选 + 定时读取的勾，
        // 都是文档自己的设值。
        return match id {
            1751 => ed.watch_mode == crate::app::WatchMode::Global,
            1752 => ed.watch_mode == crate::app::WatchMode::Prompt,
            1753 => ed.watch_mode == crate::app::WatchMode::AutoRead,
            1754 => ed.watch_mode == crate::app::WatchMode::AutoReadEnd,
            1755 => ed.watch_timed,
            _ => false,
        };
    };
    match cmd {
        Command::ViewToggle(bit) => ed.view_flags.get(bit),
        Command::ViewFullscreen => fullscreen,
        Command::WrapCancel => wrap == WrapMode::Cancel,
        Command::WrapSmart => wrap == WrapMode::Smart,
        Command::WrapWindow => wrap == WrapMode::Window,
        Command::WrapFixedColumn => wrap == WrapMode::FixedColumn,
        Command::WrapFixedColumnTabs => wrap == WrapMode::FixedColumnTabs,
        Command::WrapFixedPixel => wrap == WrapMode::FixedPixel,
        Command::EolPc => ed.eol == Eol::Crlf,
        Command::EolUnix => ed.eol == Eol::Lf,
        Command::EolMac => ed.eol == Eol::Cr,
        Command::FoldCancel => ed.folding_method == FoldingMethod::None,
        Command::FoldBySyntax => ed.folding_method == FoldingMethod::Syntax,
        Command::FoldByIndent => ed.folding_method == FoldingMethod::Indent,
        Command::ViewToolbar => chrome.toolbar,
        Command::ViewTabbar => chrome.tabbar,
        Command::ViewStatusbar => chrome.statusbar,
        Command::ViewModeBar => chrome.modebar,
        Command::HideMenu => chrome.menu,
        Command::ViewExtToolbar => chrome.exttoolbar,
        // 激活选区锚点 (1487): checked while the
        // view has an anchor armed.
        Command::SelectAnchor => ed.anchor.is_some(),
        // The highlight family (1271 / 1313 / 1327) and
        // the frame-global latches (1564 / 1651) ride on
        // Chrome; the editor's own 1654 / 1657 latches
        // fix the rows that used to read `false` here.
        Command::ColorHighlightToggle => chrome.color_highlight,
        Command::SelHighlightToggle => chrome.sel_highlight,
        Command::UrlHighlightToggle => chrome.url_highlight,
        Command::AutoPairToggle => chrome.auto_pair,
        Command::AutoIndentToggle => chrome.auto_indent,
        Command::EventScriptToggle => chrome.enable_event,
        Command::WatchClipboardToggle => chrome.watch_clipboard,
        Command::ShowEmoji => chrome.show_emoji,
        // 深色主题 (1711): the frame's own mode flag.
        Command::ViewDarkTheme => chrome.dark_theme,
        // HEX模式 (1412): checked while the active document shows in the
        // hex window — the same per-document state the toggle flips.
        Command::HexMode => ed.hex.is_some(),
        _ => false,
    }
}

/// The tab strip's own right-click menu. The close/save/file rows read like
/// the 文件 menu's, and the middle third is the 窗口 menu's split/group
/// section (1204/1205/1207/1224, 1208-1211) plus 重命名 (1648) — same command
/// ids, so a row here and its keyboard shortcut cannot disagree. Two rows know
/// the frame's mind: 合并分组 (1210) has nothing to merge while the editor area
/// is unsplit, and 分离到新实例 (1224) has nothing to hand over while the
/// document has no file behind it. 同步滚动 (1208) wears the toggle's own
/// state, and so do the two split rows: `split` is the direction the knife is
/// lying in (`None` = unsplit), and the row that matches it carries the check —
/// clicking that one again is what takes the cell away.
///
/// The last section is the 排序 popup (1272) — 按照字母升序 / 按照字母降序
/// (1272/1273) — and 设置 (1335), which opens 选项.
///
/// The shortcuts on the first block are the keys' own labels: the rows carry
/// them after a `\t`, which is the popup's accelerator column, and the keys
/// and the rows stay one table apart.
pub(super) fn tab_menu_rows(
    split: Option<bool>,
    sync_scroll: bool,
    can_detach: bool,
) -> Vec<MenuItem> {
    let row = |id: u32, label: &str| {
        MenuItem::new(id, label).with_enabled(cmdids::is_answerable(id))
    };
    vec![
        row(1206, "关闭(&C)\tCtrl+W"),
        row(1203, "关闭其它窗口(&C)\tCtrl+Shift+F4"),
        row(1202, "关闭全部窗口(&A)"),
        MenuItem::separator(),
        row(1244, "关闭左侧窗口"),
        row(1245, "关闭右侧窗口"),
        MenuItem::separator(),
        row(57603, "保存(&S)\tCtrl+S"),
        row(57604, "另存为(&A)...\tCtrl+Shift+S"),
        MenuItem::separator(),
        row(1204, "垂直分割(&V)").with_checked(split == Some(true)),
        row(1205, "水平分割(&H)").with_checked(split == Some(false)),
        row(1207, "克隆视图"),
        row(1224, "分离到新实例").with_enabled(can_detach),
        MenuItem::separator(),
        row(1209, "移动到垂直分组(&V)"),
        row(1211, "移动到水平分组(&H)"),
        row(1210, "合并分组").with_enabled(split.is_some()),
        row(1208, "同步滚动(&N)").with_checked(sync_scroll),
        MenuItem::separator(),
        row(1648, "重命名"),
        row(1650, "复制路径(&P)"),
        row(1649, "复制文件名(&N)"),
        row(1655, "在文件管理器中打开(&O)"),
        MenuItem::separator(),
        MenuItem::popup(
            1272,
            "标签排序",
            vec![row(1272, "按照字母升序"), row(1273, "按照字母降序")],
        ),
        row(1335, "设置(&S)"),
    ]
}

/// The bar as a tree of `MenuItem`s, ready for `menu::install_bar`.
/// Every command id the bar names, in menu order — the 工具条 page's
/// available list follows the menus instead of the id table, so related
/// commands sit together the way they do in the bar.
pub(super) fn command_order() -> Vec<u32> {
    fn walk(items: &[MenuItem], out: &mut Vec<u32>) {
        for item in items {
            if !item.children.is_empty() {
                walk(&item.children, out);
            } else if item.id != 0 && !item.separator && !out.contains(&item.id) {
                out.push(item.id);
            }
        }
    }
    let mut out = Vec::new();
    walk(&tree(), &mut out);
    out
}

pub(super) fn tree() -> Vec<MenuItem> {
    let mut bar = Vec::new();
    read(ROWS, &mut 0, 0, &mut bar);
    // The table is the Chinese bar as the live menu reported it; the language
    // table turns it into whatever `setting.ini` selected. Without an
    // initialized table (unit tests) this is a no-op and the captions stay.
    crate::win32::lang::localize_menu(128, &mut bar);
    bar
}

fn read(rows: &[Row], i: &mut usize, depth: u8, out: &mut Vec<MenuItem>) {
    while *i < rows.len() && rows[*i].0 == depth {
        let (_, kind, id, label) = rows[*i];
        *i += 1;
        match kind {
            SEP => out.push(MenuItem::separator()),
            LEAF => out
                .push(MenuItem::new(id, label).with_enabled(cmdids::is_answerable(id))),
            _ => {
                let mut children = Vec::new();
                read(rows, i, depth + 1, &mut children);
                // A popup parent is never greyed: choosing it opens it, and that
                // is how the user discovers which rows this implementation still owes.
                // `build_bar` appends the top level with `MF_POPUP` alone, so
                // enablement here is a command-row property only.
                out.push(MenuItem::popup(id, label, children));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_has_ten_top_level_popups() {
        let bar = tree();
        assert_eq!(bar.len(), 10);
        assert!(bar.iter().all(|p| !p.children.is_empty()));
    }

    /// Every Chinese-caption leaf in the bar needs a row in both bundled
    /// language tables: with no `[MENU128]` entry `localize_menu` leaves the
    /// Chinese literal on screen, which is exactly how 新建脚本 and 脚本包
    /// stayed Chinese on an English UI. Language-neutral captions (`80%`,
    /// `PC (CR+LF)`) need no translation and are skipped.
    #[test]
    fn every_chinese_bar_leaf_is_in_both_tables() {
        let cjk = |s: &str| s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c));
        let mut missing = Vec::new();
        for &(depth, kind, id, caption) in ROWS {
            if depth == 0 || kind != LEAF || id == 0 || !cjk(caption) {
                continue;
            }
            let zh = crate::win32::lang::builtin_bar_caption(false, id);
            let en = crate::win32::lang::builtin_bar_caption(true, id);
            let translated = matches!(&en, Some(text) if !cjk(text));
            if zh.is_none() || !translated {
                missing.push((id, caption, zh, en));
            }
        }
        assert!(missing.is_empty(), "bar rows without a translation: {missing:?}");
    }

    #[test]
    fn no_leaf_id_clashes_with_the_status_strip() {
        // The strip posts `WM_COMMAND` with its own control id in the low word,
        // so a bar row using the same number would be read as a pane click.
        let clash: Vec<u32> = ROWS
            .iter()
            .filter(|r| r.1 == LEAF && r.2 == crate::win32::statusbar::CTRL_ID)
            .map(|r| r.2)
            .collect();
        assert!(clash.is_empty(), "bar leaf reuses status id {clash:?}");
    }

    /// HEX模式 (1412) is a per-document check row: marked while the active
    /// document shows in the hex window, unmarked once the toggle hands the
    /// surface back to the text editor.
    #[test]
    fn hex_mode_checks_with_the_documents_surface() {
        let mut ed = crate::app::Editor::new();
        let chrome = no_chrome();
        assert!(!check_of(1412, &ed, WrapMode::Cancel, false, &chrome));
        ed.hex = Some(dg_hex::HexView::new());
        assert!(check_of(1412, &ed, WrapMode::Cancel, false, &chrome));
    }

    /// 显示颜文字 (1334) is the 查看 → 颜文字 check row: marked while the
    /// document draws emoji as colour glyphs.
    #[test]
    fn emoji_row_checks_with_the_render_switch() {
        let ed = crate::app::Editor::new();
        let mut chrome = no_chrome();
        assert!(!check_of(1334, &ed, WrapMode::Cancel, false, &chrome));
        chrome.show_emoji = true;
        assert!(check_of(1334, &ed, WrapMode::Cancel, false, &chrome));
    }

    /// 深色主题 (1711) marks the frame's mode, not the palette on screen: the
    /// row keeps its mark when a third theme is picked by hand.
    #[test]
    fn dark_theme_row_checks_with_the_frame_mode() {
        let ed = crate::app::Editor::new();
        let mut chrome = no_chrome();
        assert!(!check_of(1711, &ed, WrapMode::Cancel, false, &chrome));
        chrome.dark_theme = true;
        assert!(check_of(1711, &ed, WrapMode::Cancel, false, &chrome));
    }

    /// A chrome with every switch off — the rows these tests ask about read one
    /// flag each, so the rest stay down.
    fn no_chrome() -> Chrome {
        Chrome {
            toolbar: false,
            tabbar: false,
            statusbar: false,
            modebar: false,
            menu: false,
            exttoolbar: false,
            color_highlight: false,
            sel_highlight: false,
            url_highlight: false,
            auto_pair: false,
            auto_indent: false,
            enable_event: false,
            watch_clipboard: false,
            show_emoji: false,
            dark_theme: false,
        }
    }

    /// The bar position of a top-level popup is its ordinal among the
    /// depth-0 rows, not its row number in `ROWS` — `GetSubMenu` takes the
    /// former, and the two only agree for 文件.
    #[test]
    fn top_level_indices_are_bar_positions() {
        assert_eq!(file_menu_index(), 0);
        assert_eq!(tools_menu_index(), 6);
        assert_eq!(ext_menu_index(), 7);
        // and each index really names that popup in the built bar
        let bar = tree();
        assert!(bar[tools_menu_index()].label.starts_with("工具"));
        assert!(bar[ext_menu_index()].label.starts_with("扩展"));
        // the dynamic blocks start right after each menu's static rows
        assert_eq!(ext_menu_base_count(), 7);
        assert_eq!(tools_menu_base_count(), 13);
    }

    /// The tab strip's right-click menu: the rows run close/save/file, then
    /// the split/group section the 窗口 menu also carries, then rename/copy,
    /// then the sort popup and 设置 — and the rows that read the frame's mind
    /// wear its state: 合并分组 is grey while the editor area is unsplit,
    /// 分离到新实例 while the document has no file behind it, 同步滚动 and the
    /// split row of the current direction wear the toggle's mark.
    #[test]
    fn the_tab_menu_follows_the_frame_state() {
        let rows = tab_menu_rows(None, true, false);
        let ids: Vec<u32> = rows
            .iter()
            .filter(|r| !r.separator)
            .map(|r| r.id)
            .collect();
        assert_eq!(
            ids,
            vec![
                1206, 1203, 1202, // 关闭 / 关闭其它窗口 / 关闭全部窗口
                1244, 1245,     // 关闭左侧窗口 / 关闭右侧窗口
                57603, 57604,   // 保存 / 另存为
                1204, 1205, 1207, 1224, // 垂直分割 / 水平分割 / 克隆视图 / 分离到新实例
                1209, 1211, 1210, 1208, // 移动到垂直分组 / 水平分组 / 合并分组 / 同步滚动
                1648, 1650, 1649, 1655, // 重命名 / 复制路径 / 复制文件名 / 在文件管理器中打开
                1272, 1335,     // 标签排序 (popup) / 设置
            ]
        );
        let enabled = |id: u32| {
            rows.iter()
                .find(|r| r.id == id && !r.separator)
                .map(|r| r.enabled)
        };
        // No split, no file: the two rows have nothing to act on.
        assert_eq!(enabled(1210), Some(false));
        assert_eq!(enabled(1224), Some(false));
        let checked = |id: u32| {
            rows.iter()
                .find(|r| r.id == id && !r.separator)
                .map(|r| r.checked)
        };
        assert_eq!(checked(1208), Some(true));
        assert_eq!(checked(1204), Some(false), "nothing to uncheck");
        assert_eq!(checked(1205), Some(false));
        // With a split and a file both rows go live, and the sort popup's two
        // rows are the pair the tab list orders by.
        let rows = tab_menu_rows(Some(true), false, true);
        let enabled = |id: u32| {
            rows.iter()
                .find(|r| r.id == id && !r.separator)
                .map(|r| r.enabled)
        };
        assert_eq!(enabled(1210), Some(true));
        assert_eq!(enabled(1224), Some(true));
        let checked = |id: u32| {
            rows.iter()
                .find(|r| r.id == id && !r.separator)
                .map(|r| r.checked)
        };
        assert_eq!(checked(1208), Some(false));
        assert_eq!(checked(1204), Some(true), "the split's own row");
        assert_eq!(checked(1205), Some(false), "and not its twin");
        let sort = rows.iter().find(|r| r.id == 1272 && !r.children.is_empty());
        let children: Vec<u32> = sort.map(|r| r.children.iter().map(|c| c.id).collect()).unwrap();
        assert_eq!(children, vec![1272, 1273]);
    }


    /// 脚本包 (36012) 和新建脚本 (36013) 都住在扩展里；被撤掉的三行
    /// —— 扩展管理 (1569)、从本地文件安装扩展 (1567)、保存为脚本 (1568)
    /// —— 以及后来的两条内置脚本行 (36005/36006)，整张菜单表里都不再有。
    /// 设置页只留它自己的那几行。
    #[test]
    fn script_rows_live_in_the_extension_menu_alone() {
        let rows_of = |(_, start): (usize, usize)| -> Vec<u32> {
            let mut ids = Vec::new();
            for &(depth, kind, id, _) in &ROWS[start + 1..] {
                if depth == 0 {
                    break;
                }
                if depth == 1 && kind == LEAF {
                    ids.push(id);
                }
            }
            ids
        };
        let ext = rows_of(top_level("扩展").expect("扩展 row"));
        let tools = rows_of(top_level("工具").expect("工具 row"));
        for id in [36012, 36013] {
            assert!(ext.contains(&id), "id {id} 应在扩展菜单");
            assert!(!tools.contains(&id), "id {id} 不该出现在工具菜单");
        }
        for gone in [1567, 1568, 1569, 36005, 36006] {
            assert!(
                ROWS.iter().all(|&(_, _, id, _)| id != gone),
                "id {gone} 已从菜单撤掉，不该再出现在表里"
            );
        }
        // 新建脚本和脚本包是相邻的两行，都在刷新脚本菜单之前。
        let at_new = ext.iter().position(|&id| id == 36013).expect("新建脚本");
        let at_pkg = ext.iter().position(|&id| id == 36012).expect("脚本包");
        assert_eq!(at_new + 1, at_pkg, "新建脚本 和 脚本包 应相邻");
        // 扩展菜单的静态行到 刷新脚本菜单 为止：扫描与脚本命令的动态行
        // 只在运行时插在它后面。
        let at_refresh = ext.iter().position(|&id| id == 1574).expect("刷新脚本菜单");
        assert_eq!(
            at_refresh + 1,
            ext.len(),
            "刷新脚本菜单 应是扩展菜单的最后一行"
        );
    }

    /// 文件 → 新建实例 (2012) 是能点的，FTP/SFTP (1239) 已经从菜单里撤掉。
    /// 命令表还留着 1239 是因为工具条上那颗置灰的按钮要用它取名，这里查的
    /// 是菜单树，不是命令表。
    #[test]
    fn file_menu_has_new_instance_without_ftp() {
        let bar = tree();
        let rows = &bar[file_menu_index()].children;
        let at = rows
            .iter()
            .position(|r| r.id == 2012 && !r.separator)
            .expect("新建实例 row");
        assert!(rows[at].enabled, "新建实例 这一行不能是灰的");
        assert!(
            !rows.iter().any(|r| r.id == 1239 && !r.separator),
            "FTP/SFTP row should be gone"
        );
        // 这一小块前后各有一根线，删一行不该把两根线并成一根。
        assert!(rows[at - 1].separator && rows[at + 1].separator);
    }

    /// The 文件 popup is the bar's first dropdown, and it carries the two
    /// dynamic sub-popups in the table's order: 会话 before 最近的文件.
    #[test]
    fn file_menu_popups_are_measured_positions() {
        let (session, recent) = file_menu_dynamic_popups();
        assert!(session < recent, "会话 ({session}) must precede 最近的文件 ({recent})");
        // The walk found both popups by caption, not by the fallback.
        let bar = tree();
        let file = &bar[file_menu_index()];
        assert_eq!(file.children[session].label, "会话");
        assert!(file.children[recent].label.starts_with("最近的文件"));
    }

    /// The enable rules the idle update applies to the
    /// three dynamic rows; every other row in these popups is always live.
    #[test]
    fn file_menu_dynamic_rows_enable_from_the_lists() {
        // 清除历史记录 (1236) needs a recent entry.
        assert!(!row_enabled(1236, 0, 0, None));
        assert!(row_enabled(1236, 1, 0, None));
        // 清除会话菜单 (1242) needs a saved session.
        assert!(!row_enabled(1242, 5, 0, None));
        assert!(row_enabled(1242, 0, 2, None));
        // 保存到当前会话 (1243) needs a current session name.
        assert!(!row_enabled(1243, 0, 1, Some("")));
        assert!(row_enabled(1243, 0, 1, Some("work")));
        // 保存到新会话 (1241) and the dynamic rows themselves are ungated.
        assert!(row_enabled(1241, 0, 0, None));
        assert!(row_enabled(2801, 0, 0, None));
        assert!(row_enabled(35001, 0, 0, None));
    }

    /// 监视该文件变更 (1751-1755) 的勾都在文档自己的档位上：新文档默认
    /// 「使用全局监视设置」，定时读取不勾。
    #[test]
    fn monitor_rows_check_with_the_documents_mode() {
        let ed = crate::app::Editor::new();
        let chrome = no_chrome();
        assert!(check_of(1751, &ed, WrapMode::Cancel, false, &chrome));
        assert!(!check_of(1752, &ed, WrapMode::Cancel, false, &chrome));
        assert!(!check_of(1753, &ed, WrapMode::Cancel, false, &chrome));
        assert!(!check_of(1754, &ed, WrapMode::Cancel, false, &chrome));
        assert!(!check_of(1755, &ed, WrapMode::Cancel, false, &chrome));
        let mut prompted = crate::app::Editor::new();
        prompted.watch_mode = crate::app::WatchMode::AutoReadEnd;
        prompted.watch_timed = true;
        assert!(check_of(1754, &prompted, WrapMode::Cancel, false, &chrome));
        assert!(check_of(1755, &prompted, WrapMode::Cancel, false, &chrome));
    }

    /// 窗口菜单的分割行、文件菜单的监视弹层都要能在菜单树里定位：找不
    /// 到就说明菜单形状变了，WM_INITMENUPOPUP 的刷新会落空。
    #[test]
    fn split_and_monitor_popups_are_locatable() {
        assert!(window_menu_index() > 0);
        assert_ne!(monitor_popup_pos(), usize::MAX);
    }
}
