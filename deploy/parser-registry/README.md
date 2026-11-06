# 「解析器下载」服务端 —— Cloudflare Worker + 静态资源

一份注册表（`registry.json`）+ 若干包 zip（`pkg/<name>/<version>.zip`），
部署在一个 Worker 上。免费额度覆盖全部流量：静态资源请求不计数、
带宽免费，`/api/*` 走 Workers Free 的每日 10 万请求。

## 布局

    wrangler.toml        Worker 配置：/api/* 先进 Worker，其余走 public/ 静态资源
    src/worker.js        /api/query（按扩展名/包名过滤）与 /api/registry（整表转发）
    public/_headers      缓存策略：带版本号的 pkg/* 一年 immutable，registry.json 五分钟
    public/.assetsignore 排除 .gitignore 等不上线的本地文件
    public/registry.json 注册表（生成产物，不进版本库）
    public/pkg/...       包 zip（生成产物，不进版本库）

## 生成与部署

    # 1. 先有插件包目录（tools/make_parsers.py 从 Helix 发行包生成）
    python tools/make_parsers.py --helix <helix 目录>

    # 2. 生成注册表与包 zip（--check 只查扩展名冲突）
    python tools/make_registry.py --parsers parsers --out deploy/parser-registry/public

    # 3. 本地起服验证（http://localhost:8787/api/query?ext=rs）
    npx wrangler dev

    # 4. 上线（首次先 npx wrangler login）
    npx wrangler deploy

部署完成后地址是 `https://dogedit-parsers.<你的账号子域>.workers.dev`。
把这个地址填进主程序（`setting.ini` 的 `[Download] Registry`，或改
`download.rs` 里的默认常量随包发布）。自定义域可在 Cloudflare 控制台
给 Worker 绑定，客户端无感。

当前线上：`https://dogedit-parsers.xqtxyz.workers.dev`（2026-10-31 首发，
62 个包 / 118 个扩展名，与主程序 `DEFAULT_SOURCE` 一致）。重新生成注册表
后重跑一次 `npx wrangler deploy` 即可，未变的包按内容哈希跳过上传。

## 免费额度核对

- 静态资源（registry.json 与全部包 zip）：请求不计数、带宽免费；
  单文件上限 25 MiB、目录上限 20 000 个文件 —— 现有全部 grammar 打包
  约 30 MB、单包 < 6 MB，余量巨大。
- `/api/query`：Workers Free 每天 10 万次；客户端只在「打开没有解析器的
  文件」与「管理框刷新」时请求，且有本地缓存与负缓存，正常用量到不了
  零头。不部署 KV/R2/D1，因此不需要绑卡。

## 客户端消费的路径

- `GET /registry.json` —— 整表（管理框列表），带 ETag，客户端条件请求；
- `GET /api/query?ext=<ext>` —— 单扩展名候选（首开提醒），小响应；
- `GET /pkg/<name>/<version>.zip` —— 包本体，路径带版本号所以一年 immutable。

包 zip 校验三道关（客户端 `dg-syntax::install`）：sha256 对注册表声明、
zip 内路径防穿越、解包体积上限。
