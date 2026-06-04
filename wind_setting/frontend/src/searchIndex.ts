// 编译期收集：Vite 在构建时静态解析 glob，把所有 *.search.ts 清单打入包。
// 新增一页清单（如 hotkey.search.ts）即自动并入，无需改本文件。
import type { SearchEntry } from "@/schemas/searchEntry";

const modules = import.meta.glob<{ entries: SearchEntry[] }>("./pages/*.search.ts", {
  eager: true,
});

export const searchIndex: SearchEntry[] = Object.values(modules).flatMap((m) => m.entries);
