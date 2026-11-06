// 「解析器下载」的 Worker：只管 /api/* 两个查询端点，包 zip 与整表
// registry.json 都由静态资源层直接出（请求不计数、带宽免费）。
//
// /api/query?ext=<ext>   首开提醒用的小响应：按扩展名（也认 name=）过滤
//                        注册表，返回 candidates 数组。
// /api/registry          整表：转发静态资源 /registry.json，ETag 与条件
//                        请求由静态资源层自带，客户端拿去增量刷新。
//
// 注册表从自己的静态资源里读一次，缓存在 isolate 内存里 —— 部署新
// 版本会换 isolate，缓存自然失效，没有手动清的问题。

let cachedRegistry = null;

async function loadRegistry(env) {
  if (cachedRegistry) {
    return cachedRegistry;
  }
  const res = await env.ASSETS.fetch("https://assets.internal/registry.json");
  if (!res.ok) {
    throw new Error("registry.json 不可用: " + res.status);
  }
  cachedRegistry = await res.json();
  return cachedRegistry;
}

function json(obj, status) {
  return new Response(JSON.stringify(obj), {
    status,
    headers: { "content-type": "application/json; charset=utf-8" },
  });
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname === "/api/query") {
      const ext = (url.searchParams.get("ext") || "").trim().toLowerCase().replace(/^\./, "");
      const name = (url.searchParams.get("name") || "").trim().toLowerCase();
      if (!ext && !name) {
        return json({ error: "需要 ext= 或 name=" }, 400);
      }
      let registry;
      try {
        registry = await loadRegistry(env);
      } catch (e) {
        return json({ error: String(e) }, 500);
      }
      const results = registry.packages.filter(
        (p) =>
          (ext && (p.extensions || []).includes(ext)) ||
          (name &&
            (p.name === name ||
              String(p.display || "").toLowerCase() === name))
      );
      return json({ query: ext || name, results });
    }
    if (url.pathname === "/api/registry") {
      return env.ASSETS.fetch(new URL("/registry.json", url.origin));
    }
    return json({ error: "unknown api path" }, 404);
  },
};
