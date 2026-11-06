# 原生插件 API 方案（zero-copy）

> 目标：为「性能攸关」的场景提供进程内原生插件 API。插件可以 **zero-copy 借用**  
> 编辑器内部数据结构——文本缓冲区（raw 字节块、行数组）、tree-sitter 语法树——  
> 而不必自己维护一份镜像或反复拷贝。
>
> 与现有两套扩展机制的分工：  
> Lua 脚本系统 = 安全沙箱 + 拷贝语义 + 易用；`parsers\` 语法插件包 = 只导出一个  
> grammar 函数的语法来源；原生插件 = 消费宿主 API、直接读写内部结构的全能力插件。
>
> 技术基线：tree-sitter 0.27。

## 0. 结论摘要

| 决策点                   | 选型                                                                          |
| --------------------- | --------------------------------------------------------------------------- |
| 定位                    | 进程内、信任模型同 `parsers\`（同 Neovim），v1 不做隔离                                      |
| ABI                   | **手写 C ABI**（`extern "C"` + 版本化结构 + 函数指针表），不用 abi_stable/stabby             |
| 插件形态                  | 目录式插件包 `plugins\<name>\`：`manifest.toml` + `<name>.dll`（复用 parsers\ 约定）     |
| 加载                    | 启动只扫 manifest；首次用到时 `LoadLibraryW`；**永不 FreeLibrary**（沿用 dynlang 铁律）        |
| 数据访问核心                | **两级模型**：L1 借用（回调内有效的零成本指针）+ L2 快照（引用计数，可跨事件/线程）                            |
| 文本 zero-copy          | raw 域整文件字节块（现成 `Arc<Vec<u8>>`）+ 行数组（`LineT*` 本身就是 repr(C)）+ 行快照（`Rc`）       |
| tree-sitter zero-copy | HostApi vtable **转发 tree_sitter C ABI 函数指针** + 按值借出 `TSNode`/`TSTreeCursor` |
| 线程契约                  | 回调默认 UI 线程（与 Document 单所有权一致）；后台线程只允许持有 L2 快照                               |
| 写路径                   | v1 只开放批量事务编辑 API（走 undo），不开放直接内存写                                           |
| 失效控制                  | 借用只在回调内存活（dg-script `BridgePtr` 协议同款）；快照引用计数、永不悬垂                           |
| 崩溃                    | v1 进程同生共死 + VEH 记录肇事插件名；v3 可选 out-of-proc 宿主（可共享基建）                         |

## 1. 与现有两套扩展机制的边界

|      | Lua 脚本（dg-script）  | parsers\ 语法包    | 原生插件（本文）                |
| ---- | ------------------ | --------------- | ----------------------- |
| 目标用户 | 自动化/个人定制           | 语法作者            | 性能工具作者                  |
| 语言   | Lua 5.4（mlua）      | 任意（C ABI DLL）   | 任意（C ABI DLL）           |
| 数据语义 | 每次调用**拷贝** String  | 不接触文本           | **zero-copy 借用/快照**     |
| 沙箱   | 有（无 os/io/package） | 无（信任）           | 无（信任）                   |
| 暴露结构 | 无                  | grammar 函数指针    | Document/Tree/事件        |
| 崩溃影响 | handler 报错继续跑      | grammar 崩 = 进程崩 | 崩 = 进程崩（VEH 记名）         |
| 典型用例 | 批量替换文本、菜单命令        | 语法高亮/折叠         | LSP 桥、linter、二进制分析、语义导航 |

三者不互斥：原生插件与脚本共用 storage（后续可议）、共用命令/菜单注册通道；  
一个语法包 + 一个原生插件可以同时服务于同一文件。

## 2. 现状事实（调研结论，含行号）

### 2.1 文本缓冲区

- `dg-core/src/linedoc.rs` 按行为重写了 `LineVector`：
  - `LineT`（L54-72）`#[repr(C, align(8))]` 24 字节：`len: u32, cap: u32, data: u64`，  
    有 `size_of == 24` 断言（L1020）。**它本身就是 C 布局**。
  - 双域：`cap == 0` → raw 未解码行，`data` 是指向整文件字节块  
    `raw: Option<Arc<Vec<u8>>>`（L368）内的偏移；`cap != 0` → owned 行，  
    `data` 是 UTF-32 `char` 堆缓冲指针。
  - `LineVector` 是对 `LineT` 记录数组的 gap buffer（L355-380）；gap 移动是  
    memmove（L538），owned 行插入走倍增 realloc（L116）——**无地址稳定保证**，  
    `LineView` 借用被明确要求不得跨编辑存活（L676 注释）。
- `Document`（document.rs L273）= `Mem(MemDoc) | Large(Box<LargeDoc>)`。  
  `MemDoc`（L242-264）内含 `LineVector` + `RwLock<LineCache>` + `RwLock<ByteIndex>`；  
  视图缓存 `views: RefCell<HashMap<usize, Rc<Vec<char>>>>`（linedoc.rs L345）——  
  **解码后的行以 `Rc` 临时共享，编辑时整表 clear，Rc 由持有者决定生死**。
- 现成的零拷贝读取口：`raw_line_bytes`（linedoc.rs L738，raw 行直接借原始文件字节）、  
  loader 的 `Arc<Vec<u8>>` 直入 buffer（loader.rs L60-88，零解码）。
- 内部文本域是 UTF-32 `char`；**UTF-8 视图在编辑过的行上需要按需物化**。

### 2.2 tree-sitter

- `Highlighter`（dg-syntax/highlight.rs L107）独占 `Tree/Parser/Query`，每 Editor 一份，  
  挂在 `Editor` 上（app.rs L309），UI 线程独占 `&mut`。
- 编辑到达 → `highlighter_note_edit`（app.rs L1093）先 `tree.edit()` 平移偏移  
  （`fold_pending`，highlight.rs L221），重 parse 由 WM_TIMER 去抖触发  
  `sync_syntax`（app.rs L1044，window.rs L18417）→ `parse_feed` 按 ~64KiB chunk  
  **拷出**文本喂给 parser。增量 parse 复用旧树。
- 高亮 query 每帧每行重跑（highlight.rs L342/L394，无跨帧缓存）；  
  文本经 `node_text` 回调按需拷出（app.rs L82 `doc_slice`）。
- `dynlang.rs::load_parser`（L60）：libloading + 导出符号 +  
  `LanguageFn` + ABI 区间校验 + 空文档冒烟 parse；**永不 FreeLibrary**，  
  句柄收养进进程级 `LIVED_LIBS`。

### 2.3 线程与锁

- `Document` 按值内嵌 `Editor`（app.rs L303），**UI 线程单所有权、无外层锁**；  
  全代码库没有 `Arc<Document>`。后台任务只通过 crossbeam channel 收发 owned 数据  
  （window.rs L865、L2834 等）。
- 无 revision/epoch 计数器；失效靠「突变时清缓存 + mark_dirty」。

### 2.4 可复用的先例

1. **`BridgePtr` 协议**（dg-script lib.rs L129/L530）：宿主对象以裸指针借给引擎，  
   只在 entry-point 栈帧内有效，返回前清槽。「回调内借用」的安全模板。
2. **dynlang 铁律**：永不卸载 DLL + ABI 区间校验 + 冒烟测试 + `manifest.abi` 字段  
   （manifest.rs L33）——原生插件全部沿用。
3. **loader 的 `Arc<Vec<u8>>`**：加载后未编辑过的文档，raw 域字节天然稳定可共享。
4. **parsers\ 三扫描根约定**（exe 旁 / 工作目录 / `%APPDATA%`）：目录布局照搬。

## 3. 零拷贝为什么可行：四个关键洞察

1. **raw 域已经是 `Arc`**。加载即 `Arc<Vec<u8>>`，未编辑区域可以直接把  
   `&[u8]`（指针+长度）交给插件，引用计数 +1 即可，零转换零拷贝。
2. **`Rc` 行视图缓存给出了「快照」语义的现成实现**。借出 `Rc<Vec<char>>` 的克隆  
   = 借走那一刻的数据快照：宿主后续编辑产生新版本，旧块只要插件还持有就不会  
   被释放。**悬垂问题在数据结构层面消失**，不需要 epoch 校验。
3. **tree-sitter 本来就是 C ABI**。`TSNode`/`TSTreeCursor` 是官方 C 结构（按值传递），  
   宿主在自己的 vtable 里放一组转发 tree_sitter 运行时的函数指针  
   （`node_child`/`cursor_goto_first_child`/…），插件拿到 `TSNode` 值即可遍历  
   当前语法树——**不需要链接 tree-sitter，也不需要把树序列化/镜像**。  
   顺带收益：未来 out-of-proc 时可替换转发实现。
4. **UI 线程串行 + 回调模型 = 借用免费**。所有宿主回调都在 UI 线程派发，  
   借用指针只在回调栈帧内有效，宿主不存在「一边编辑一边被读」的窗口，  
   与 `&Document` 现有借用模型、`BridgePtr` 协议完全同构，无需新增锁。

安全核心浓缩成一句话：

> **借出的裸指针只在回调栈帧内有效；要跨事件/跨线程，必须显式拿 L2 快照  
> （引用计数对象或深拷贝）。宿主在每次回调返回时回收 L1。**

## 4. ABI 设计

### 4.1 总体形态

- 插件导出唯一入口：`dg_plugin_descriptor()`，返回 `*const EvPluginDescriptor`  
  （静态 const 表）。宿主读取后调用其 `on_load(host, ud)`。
- 宿主交给插件一个版本化函数指针表 `EvHostApiVt`（单表，尾部可追加；  
  每个结构体首字段 `u32 size`，Windows 风格扩展）。
- `DG_PLUGIN_ABI_VERSION = 1`；manifest 写 `abi = [lo, hi]` 区间  
  （语义同 parsers\ 的 manifest.abi，快速拒绝，真正校验在加载时）。

### 4.2 头文件草案（节选）

```c
/* dg_plugin.h — 手写 C ABI；Rust 侧由 crates/plugin-api 手写镜像类型
   并用测试断言 size_of/align_of 一致。 */

#define DG_ABI_VERSION 1
#define DG_API_VERSION_MIN 1
#define DG_API_VERSION_MAX 1

typedef uint32_t dg_doc_id_t;      /* 代次化句柄：低 20 位 id，高 12 位代次 */
typedef struct dg_host_t dg_host_t;

/* ---- L1 借用（只在回调栈帧内有效） ---- */
typedef struct dg_slice_u8  { const uint8_t* ptr; uint32_t len; uint32_t _pad; } dg_slice_u8_t;
typedef struct dg_line_row  { uint32_t len; uint32_t cap; uint64_t data; uint8_t flags[4]; uint8_t _pad[4]; } dg_line_row_t;
/* dg_line_row_t 即内部 LineT 的镜像：cap==0 时 data=raw 块内偏移，cap!=0 时 data=UTF-32 char* */

/* ---- L2 快照（引用计数，跨事件/线程安全） ---- */
typedef struct dg_snap_u8 { const uint8_t* ptr; uint32_t len; /* 不透明 */ uint64_t _r[3]; } dg_snap_u8_t;

/* ---- tree-sitter 借用（值类型，来自 tree_sitter/api.h 的官方布局） ---- */
typedef struct { uint64_t _v[4]; } dg_ts_node_t;      /* TSNode 按值拷贝 */
typedef struct { uint64_t _v[8]; } dg_ts_cursor_t;    /* TSTreeCursor 按值拷贝 */

typedef struct dg_host_vt {
    uint32_t size;                    /* = sizeof(dg_host_vt)，尾部扩展 */
    uint32_t abi_version;
    void*    ud;                      /* 宿主上下文，回传给每个函数 */

    /* 日志/杂项 */
    void   (*log)(dg_host_t*, const char* msg, uint32_t len);

    /* 文档（dg_doc_id_t 失效时返回 0/null） */
    dg_doc_id_t (*doc_current)(dg_host_t*);              /* 活动文档 */
    uint32_t    (*doc_line_count)(dg_host_t*, dg_doc_id_t);
    dg_slice_u8 (*doc_raw_bytes)(dg_host_t*, dg_doc_id_t);          /* L1；空文档 null */
    dg_snap_u8  (*doc_snap_raw)(dg_host_t*, dg_doc_id_t);           /* L2 整文快照 */
    uint32_t    (*doc_snap_line)(dg_host_t*, dg_doc_id_t,
                                 uint32_t line,                      /* 0-based */
                                 const uint32_t** chars_out);        /* L2 UTF-32 行快照 */
    uint32_t    (*doc_snap_line_utf8)(dg_host_t*, dg_doc_id_t,
                                 uint32_t line, const uint8_t** bytes_out); /* v2 */

    /* tree-sitter 转发（abi_version>=1；树在回调栈帧内保证不变） */
    dg_ts_node_t  (*tree_root)(dg_host_t*, dg_doc_id_t);
    uint64_t      (*tree_epoch)(dg_host_t*, dg_doc_id_t);   /* 编辑即变化 */
    dg_ts_node_t  (*ts_node_child)(dg_host_t*, dg_ts_node_t, uint32_t idx);
    uint32_t      (*ts_node_child_count)(dg_host_t*, dg_ts_node_t);
    uint32_t      (*ts_node_start_byte)(dg_host_t*, dg_ts_node_t);
    const char*   (*ts_node_type)(dg_host_t*, dg_ts_node_t);
    dg_ts_cursor_t(*ts_cursor_new)(dg_host_t*, dg_ts_node_t);
    dg_ts_node_t  (*ts_cursor_cur)(dg_host_t*, dg_ts_cursor_t*);
    int           (*ts_cursor_down)(dg_host_t*, dg_ts_cursor_t*);
    int           (*ts_cursor_next)(dg_host_t*, dg_ts_cursor_t*);
    int           (*ts_cursor_up)(dg_host_t*, dg_ts_cursor_t*);

    /* 编辑：一批 op = 一个 undo 事务（v1 唯一写路径） */
    typedef struct { uint32_t kind; uint32_t s_line, s_col, e_line, e_col;
                     const uint8_t* text; uint32_t text_len; } dg_edit_op_t;
    int (*doc_edit)(dg_host_t*, dg_doc_id_t, const dg_edit_op_t* ops, uint32_t n_ops);

    /* 注册（低频，拷贝无妨） */
    int (*command_register)(dg_host_t*, const char* name, void* ud,
                            void (*cb)(void* ud, dg_host_t*));
} dg_host_vt;

typedef struct dg_host_t { const dg_host_vt* vt; } dg_host_t;

/* ---- 插件导出的静态描述符 ---- */
typedef struct dg_plugin_desc {
    uint32_t size;                 /* = sizeof(dg_plugin_desc) */
    uint32_t abi_min, abi_max;     /* 接受的 DG_ABI_VERSION 区间 */
    const char* name;              /* 与 manifest.name 一致 */
    const char* version;
    /* 事件回调（可空 = 不关心） */
    void (*on_load)  (dg_host_t*, void* ud);
    void (*on_unload)(dg_host_t*, void* ud);            /* v1 = 仅进程退出前 */
    void (*on_doc_open) (dg_host_t*, void* ud, dg_doc_id_t, const char* path);
    void (*on_doc_edit) (dg_host_t*, void* ud, dg_doc_id_t, uint64_t epoch,
                         const dg_edit_op_t* ops, uint32_t n_ops);
    void (*on_doc_close)(dg_host_t*, void* ud, dg_doc_id_t);
    void (*on_idle)     (dg_host_t*, void* ud, uint32_t ms_since_last);
} dg_plugin_desc;

/* 唯一必须导出的符号 */
const dg_plugin_desc* dg_plugin_descriptor(void);
```

要点：

- **回调默认 UI 线程**。`on_doc_edit` 带入参 `ops`（编辑内容本身），插件多数情况  
  无需回读文档即可增量维护自己的索引——这正是「不维护镜像」的反向服务：  
  宿主把增量喂给插件，插件持有的是它自己真正需要的东西。
- `tree_epoch`：单调计数器，每次 `fold_pending`/`parse_feed`/attach 时递增。  
  现有代码只有 `syntax_version` 计数器（app.rs L1191 附近），需要把语义统一成  
  「树代次」，这是本方案对宿主的最小侵入改动。
- `dg_ts_node_t` 的布局断言（size/align）在 dg-plugin-api 测试中与  
  `tree_sitter::ffi::TSNode` 对齐校验。

### 4.3 为什么不用 abi_stable / stabby

- parsers\ 生态（Helix grammar DLL）已经证明纯 C ABI 模式在这个代码库成立，  
  且 `LineT` 本身就是 repr(C)。原生插件暴露的核心正是这些 repr(C) 结构。
- abi_stable 只服务 Rust↔Rust，引入重型依赖与 trait 桥；stabby 更年轻。  
  手写 C ABI 的样板成本一次性（dg-plugin-api 一个 crate），换来 C/C++/Zig/Rust  
  全语言可用——编辑器插件生态值得这个成本。
- 版本化靠：描述符/结构体首字段 `u32 size` + `abi` 区间（两处已有先例）。

## 5. 两级数据访问模型（安全核心）

### 5.1 L1 借用（零成本，回调栈帧内）

| 借出物                                                | 底层                 | 失效时机                                |
| -------------------------------------------------- | ------------------ | ----------------------------------- |
| `doc_raw_bytes` → `dg_slice_u8`                    | `Arc<Vec<u8>>` 内指针 | 文件重载/关闭（回调内不会发生）                    |
| 行数组直通（v2，`doc_line_rows` → `const dg_line_row_t*`） | `LineVector` 的记录数组 | 任何结构编辑；回调内不会发生                      |
| `tree_root` → `dg_ts_node_t`                       | 当前 `Tree`          | `fold_pending`/`parse_feed`；回调内不会发生 |

L1 的正确性由线程模型保证：宿主在 UI 线程串行派发回调，回调执行期间宿主  
不做任何编辑/重解析。**宿主在每次回调返回后视全部 L1 为失效**，  
debug 构建在宿主侧放毒（回收后把已知借出指针页填 0xDD 的调试堆方案可选）  
以便抓插件违规。

### 5.2 L2 快照（引用计数，可跨事件/线程）

| 快照                            | 底层                                        | 成本             | 语义         |
| ----------------------------- | ----------------------------------------- | -------------- | ---------- |
| `doc_snap_raw` → `dg_snap_u8` | 克隆 `Arc<Vec<u8>>`                         | 1 次原子计数        | 借走时整文件原始字节 |
| `doc_snap_line`（UTF-32）       | 克隆 `Rc<Vec<char>>`（跨线程场景用 `Arc` 版缓存，见 §7） | 1 次计数 + 可能一次解码 | 借走时该行内容    |
| Tree 深拷贝（v2，`tree_snap`）      | `Tree::clone`                             | O(n) 拷贝        | 借走时的完整树    |


L2 返回的对象**永不悬垂**：宿主编辑产生新版本，旧块由引用计数兜底。
插件后台线程可以自由持有/处理 L2（例如把整文快照丢给自己的 worker 扫描）。
这是「插件不用自己维护一份数据」的主通道：插件最多持有一个引用计数快照，
而不是一份重解码/重物化的镜像。

### 5.3 写路径

v1 不开放直接内存写（undo 一致性、行 gap 结构、多重缓存的失效耦合太大）。
高性能批量编辑走 `doc_edit`：一批 op 一次调用 = 一个 undo 事务、一次
`mark_dirty`、一次事件广播。参考量级：现状脚本逐行 insert 每次都要过
Bridge + 行定位；事务化后一次 FFI 完成全部。

## 6. API 面 v1 范围

按性能诉求排序：

1. **读文本**：`doc_raw_bytes`（L1/L2）、`doc_snap_line`（UTF-32）、行数、活动文档句柄。
2. **读树**：`tree_root` + `ts_*` 转发集 + `tree_epoch`。
3. **写**：`doc_edit` 事务。
4. **事件**：doc_open / doc_edit / doc_close / idle。
5. **注册**：命令（进现有命令/菜单通道）、日志。
6. 不进 v1：渲染 hook、按键拦截、storage、面板/UI 扩展、query（highlights.scm）执行、
   多光标/选区读、LSP。

## 7. 插件包布局与生命周期

```
plugins\                       # 三扫描根，同 parsers\：exe 旁 / 工作目录 / %APPDATA%\dogedit\plugins
  lsp-bridge\
    manifest.toml
    lsp_bridge.dll
```

```toml
# manifest.toml —— 字段语义与 parsers\ 的 manifest 对齐（manifest.rs L11）
name = "lsp-bridge"            # [a-z0-9_-]，= 目录名
display = "LSP Bridge"
version = "0.1.0"
entry = "dg_plugin_descriptor" # 缺省即此
dll = "lsp_bridge.dll"         # 缺省 = {name}.dll；禁路径分隔符（同 parsers\ 校验）
abi = [1, 1]                   # 接受的 DG_ABI_VERSION 区间
```

加载流程：启动只扫 manifest → 校验 abi 区间 → **首次触发相关事件时**惰性
`LoadLibraryW` → 取 `entry` 符号 → descriptor size/abi 校验 → `on_load(host, ud)`。
**永不 FreeLibrary**（借出的树指针/事件表都指向 DLL 内代码，沿 dynlang 铁律
收养进进程级存活表）。rescan 只新增；换 DLL 需重启。

Rust 侧 crate 划分：

- `crates/plugin-api`：手写 C 镜像类型 + 常量 + 布局断言测试
  （`LineT` 24B / `TSNode` size+align / `Tree` 代次接口），无运行时逻辑。
- `crates/app`（宿主实现，后续可拆 `dg-plugin-host`）：扫描/加载/事件派发/
  VEH 记名（Vectored Exception Handler 捕获插件回调内的异常，落日志后放行崩溃）。
- `crates/fixture-native-dll`：CI 夹具（对齐 `fixture-json-dll` 模式），
  走真 LoadLibrary 链路：读 raw bytes、遍历树、提事务编辑、验证租借回收。

## 8. 坐标系（统一约定）

| 系统 | 行 | 列 |
|---|---|---|
| dg-script（Lua） | 1-based | 1-based UTF-8 byte col |
| 内部 Document | 0-based | char col（UTF-32 域）/ 内部 ByteIndex 供字节域 |
| tree-sitter | 0-based point.row | point.column（UTF-8 byte） |
| **原生插件 v1** | **0-based** | **UTF-8 byte offset（文档全域）+ point{row, byte_col}** |

原生 API 与 tree-sitter 对齐（高性能消费者主要是 tree-sitter 系工具），
文档在 dg-plugin-api 里给出三坐标系换算表，宿主负责转换。

## 9. 实施里程碑

1. **M1（骨架）**：dg-plugin-api 类型与断言 → 宿主扫描/加载/abi 校验 →
   `on_load`/`log`/`command_register` 打通，fixture-native-dll 冒烟。
2. **M2（零拷贝读）**：`doc_raw_bytes`（L1+L2）、`doc_snap_line`、
   `tree_epoch` 语义落地 → fixture 验证「编辑后 L1 失效、L2 快照稳定」。
3. **M3（树）**：`ts_*` 转发集 + `tree_root` → fixture 遍历 json 树做符号统计。
4. **M4（写与事件）**：`doc_edit` 事务 + `on_doc_edit` 增量广播 →
   fixture 做一个「实时统计光标所在函数行数」类端到端示例。
5. 对照验证：`query_sweep`/既有测试全绿；`_burn_probe` 无回归。

## 10. v2 / v3 演进

- **v2**：行数组直通（`const dg_line_row_t*` + count）、UTF-8 行缓存快照
  （给 ViewCache 加 `Rc<Vec<u8>>` 域）、`tree_snap` 深拷贝、渲染 hook
  （拦截/附加 `HighlightRun` 生成）、后台线程事件标记（`DG_THREAD_BG`）、
  query 执行转发（插件自带 highlights.scm 由宿主跑）、storage 共享。
- **v3**：out-of-proc 宿主（与 parser_plugin_plan §隔离 的 v3 共享基建）：
  快照经共享内存移交，事件改 IPC；L1 借用退化为按页映射。

## 11. 风险与开放问题

1. **`Tree` 的 raw 指针获取**：tree_sitter 0.27 未公开 `TSTree*` 访问器。
   本方案用「`TSNode`/`TSTreeCursor` 值传递 + vtable 转发」绕开了它；
   若后续需要 `ts_node_string` 之外的 tree 级操作（如 `ts_tree_copy`），
   需给 tree-sitter 打一行补丁或 reference 修改。
2. **`views: RefCell` 非 Send**：L2 行快照若要在插件线程生成，需把行快照缓存
   做成 `Arc` 版本或快照生成固定在 UI 线程回调内（v1 采取后者）。
3. **`doc_edit` 与现有 undo/宏路径的整合**：必须复用 `Editor` 现有 undo 事务
   入口，避免出现旁路编辑历史；M4 时核对。
4. **插件把 L1 指针缓存到全局**：信任模型下无法根除；缓解 = 文档明确契约 +
   debug 宿主放毒 + VEH 记名。v3 out-of-proc 后自然根除。
5. **大文件**：`Document::Large`（mmap 域）v1 不借 raw 指针（follow-tail 会
   重映射），只提供行快照；raw 直通属 v2。
