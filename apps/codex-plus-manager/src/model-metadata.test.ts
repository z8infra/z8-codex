import assert from "node:assert";
import { describe, it } from "node:test";
import { isValidAutoCompactPercent, normalizeAutoCompactEditing, normalizeAutoCompactPercent } from "./auto-compact.ts";
import {
  clearModelMetadataForSlug,
  modelMetadataKey,
  modelSlugFromRowName,
  parseModelMetadataDocument,
  parseModelMetadataMap,
  remapModelMetadataSlugs,
  replaceModelMetadataForSlug,
  retainModelMetadataForSlugs,
  serializeModelMetadataDocument,
  suffixWindowString,
  synchronizeModelMetadataDocumentContextWindow,
  synchronizeModelMetadataDocumentLimits,
  synchronizeModelMetadataDocumentLimitsPreview,
} from "./model-metadata.ts";

describe("model metadata helpers", () => {
  it("模型后缀与 Rust 一致且无效后缀不会改变身份", () => {
    for (const [row, slug, window] of [
      [" GPT-6.1-Sol[1M] ", "GPT-6.1-Sol", "1000000"],
      ["Model-X[256k]", "Model-X", "256000"],
      ["Model-X[ +256 K ]", "Model-X", "256000"],
      ["Model-X[18446744073709551615]", "Model-X", "18446744073709551615"],
      ["Model-X[18446744073709551616]", "Model-X[18446744073709551616]", null],
      ["Model-X[0]", "Model-X[0]", null],
      ["Model-X[bad]", "Model-X[bad]", null],
      [" [1M] ", "[1M]", null],
    ] as const) {
      assert.strictEqual(modelSlugFromRowName(row), slug);
      assert.strictEqual(suffixWindowString(row), window);
    }
    assert.strictEqual(modelMetadataKey("GPT-6.1-Sol[1M]"), "gpt-6.1-sol");
    assert.strictEqual(modelMetadataKey("ÄBC"), "Äbc");
  });

  it("大小写与后缀变体共用 metadata key，旧配置最后一条生效", () => {
    const value = '{"Model-X":{"description":"old"},"model-x[512K]":{"description":"latest"}}';
    assert.deepStrictEqual(parseModelMetadataMap(value), { "model-x": { description: "latest" } });
    assert.deepStrictEqual(parseModelMetadataMap('{"model-x":{"description":"first"},"MODEL-X":{"description":"middle"},"model-x":{"description":"last"}}'), {
      "model-x": { description: "middle" },
    });
    assert.deepStrictEqual(parseModelMetadataMap('{"1[1M]":{"description":"first"},"1":{"description":"last"}}'), {
      "1": { description: "first" },
    });
    assert.deepStrictEqual(parseModelMetadataMap('{"model-x":{"description":"old"},"MODEL-X":{}}'), {});
    assert.deepStrictEqual(parseModelMetadataMap('{"model-x":{},"MODEL-X":{"description":"latest"}}'), {
      "model-x": { description: "latest" },
    });
    assert.deepStrictEqual(JSON.parse(retainModelMetadataForSlugs(value, ["MODEL-X[1M]"])), {
      "model-x": { description: "latest" },
    });
    const updated = replaceModelMetadataForSlug(value, "Model-X[1M]", { description: "updated" });
    assert.deepStrictEqual(JSON.parse(updated), { "model-x": { description: "updated" } });
    assert.strictEqual(clearModelMetadataForSlug(updated, "MODEL-X[256K]"), "");
    assert.deepStrictEqual(JSON.parse(remapModelMetadataSlugs(value, [
      { previousSlug: "MODEL-X[512K]", nextSlug: "Model-Y[1M]" },
    ])), { "model-y": { description: "latest" } });
  });

  it("已有显示名在大小写变体导入后保留", () => {
    const saved = replaceModelMetadataForSlug('{"Model-X":{"display_name":"User title"}}', "MODEL-X[1M]", {
      display_name: "Vendor title", description: "Imported",
    });
    assert.deepStrictEqual(JSON.parse(saved), {
      "model-x": { display_name: "User title", description: "Imported" },
    });
  });

  it("导入与双向同步匹配无后缀模型，同时保持用户模型拼写", () => {
    const source = '{"slug":"model-x","context_window":1000000,"auto_compact_token_limit":800000,"description":"vendor"}';
    const parsed = parseModelMetadataDocument(source, "Model-X[1M]");
    assert.strictEqual(parsed.ok, true);
    if (!parsed.ok) return;
    assert.strictEqual(parsed.value.slug, "Model-X");
    assert.strictEqual(parsed.value.autoCompactPercent, "80%");
    const synchronized = synchronizeModelMetadataDocumentLimitsPreview(source, "Model-X[1M]", "512K", "90%");
    assert.ok(synchronized);
    assert.strictEqual(JSON.parse(synchronized.document).slug, "model-x");
    assert.strictEqual(synchronized.preview.contextWindow, "512000");
    assert.strictEqual(synchronized.preview.autoCompactPercent, "90%");
    const serialized = JSON.parse(serializeModelMetadataDocument("Model-X[1M]", parsed.value.metadata, "1M"));
    assert.strictEqual(serialized.models[0].slug, "Model-X");
    assert.strictEqual(parseModelMetadataDocument(JSON.stringify(serialized), "Model-X[1M]").ok, true);
  });

  it("大小写重复模型仍报歧义且不会任意选中", () => {
    const source = '{"models":[{"slug":"model-x"},{"slug":"MODEL-X"}]}';
    const parsed = parseModelMetadataDocument(source, "Model-X[1M]");
    assert.strictEqual(parsed.ok, false);
    if (!parsed.ok) assert.match(parsed.error, /多个 slug/);
    assert.strictEqual(synchronizeModelMetadataDocumentLimits(source, "Model-X[1M]", "1M", "90%"), null);
  });

  it("自动压缩编辑把数字保持在百分号前并允许清空", () => {
    assert.strictEqual(normalizeAutoCompactEditing("90%5", "90%"), "905%");
    assert.strictEqual(normalizeAutoCompactEditing("9%", "90%"), "9");
    assert.strictEqual(normalizeAutoCompactEditing("90", "90%"), "90");
    assert.strictEqual(normalizeAutoCompactEditing("", "90%"), "");
  });

  it("解析单模型并保留供应商字段", () => {
    const result = parseModelMetadataDocument(JSON.stringify({
      slug: "model-a",
      context_window: 1_000_000,
      auto_compact_token_limit: 800_000,
      max_context_window: 1_000_000,
      priority: 2,
      truncation_policy: { mode: "tokens", limit: 10000 },
      vendor_extension: ["kept"],
    }), "model-a");
    assert.strictEqual(result.ok, true);
    if (!result.ok) return;
    assert.strictEqual(result.value.contextWindow, "1000000");
    assert.strictEqual(result.value.autoCompactPercent, "80%");
    // 窗口字段由「上下文窗口」列统一管辖，不进 metadata map（issue #2191）。
    assert.deepStrictEqual(result.value.metadata, {
      priority: 2,
      truncation_policy: { mode: "tokens", limit: 10000 },
      vendor_extension: ["kept"],
    });
    assert.deepStrictEqual(result.value.ignoredFields, []);
  });

  it("max_context_window 优先于 context_window", () => {
    const result = parseModelMetadataDocument(JSON.stringify({
      slug: "model-a",
      context_window: 272_000,
      max_context_window: 1_000_000,
    }), "model-a");
    assert.strictEqual(result.ok, true);
    if (!result.ok) return;
    assert.strictEqual(result.value.contextWindow, "1000000");
    assert.deepStrictEqual(result.value.metadata, {});
  });

  it("仅 max_context_window 的文档也能提取窗口", () => {
    const result = parseModelMetadataDocument(JSON.stringify({
      slug: "model-a",
      max_context_window: 600_000,
    }), "model-a");
    assert.strictEqual(result.ok, true);
    if (!result.ok) return;
    assert.strictEqual(result.value.contextWindow, "600000");
    assert.deepStrictEqual(result.value.metadata, {});
  });

  it("仅 max_context_window 时压缩百分比按该窗口计算", () => {
    const result = parseModelMetadataDocument(JSON.stringify({
      slug: "model-a",
      max_context_window: 1_000_000,
      auto_compact_token_limit: 800_000,
    }), "model-a");
    assert.strictEqual(result.ok, true);
    if (!result.ok) return;
    assert.strictEqual(result.value.contextWindow, "1000000");
    assert.strictEqual(result.value.autoCompactPercent, "80%");
    assert.deepStrictEqual(result.value.metadata, {});
  });

  it("窗口字段为非法值时报出对应字段名", () => {
    const result = parseModelMetadataDocument(
      '{"slug":"model-a","context_window":1,"max_context_window":-5}',
      "model-a",
    );
    assert.strictEqual(result.ok, false);
    if (result.ok) return;
    assert.match(result.error, /max_context_window/);
  });

  it("编辑窗口时同步文档里的 max_context_window", () => {
    const synchronized = synchronizeModelMetadataDocumentLimits(
      '{"slug":"model-a","context_window":272000,"max_context_window":1000000,"vendor":true}',
      "model-a",
      "600000",
      "",
    );
    assert.deepStrictEqual(JSON.parse(synchronized ?? "null"), {
      slug: "model-a",
      context_window: 600_000,
      max_context_window: 600_000,
      vendor: true,
    });
  });

  it("编辑窗口清空时文档里的 max_context_window 同步置 null", () => {
    const synchronized = synchronizeModelMetadataDocumentContextWindow(
      '{"slug":"model-a","context_window":100,"max_context_window":100}',
      "model-a",
      "",
    );
    assert.deepStrictEqual(JSON.parse(synchronized ?? "null"), {
      slug: "model-a",
      context_window: null,
      max_context_window: null,
    });
  });

  it("支持 export/module 包装但不会执行 JavaScript", () => {
    assert.strictEqual(
      parseModelMetadataDocument('export default {"slug":"model-a"};', "model-a").ok,
      true,
    );
    assert.strictEqual(
      parseModelMetadataDocument('module.exports = {"models":[{"slug":"model-a"}]};', "model-a").ok,
      true,
    );
    assert.strictEqual(parseModelMetadataDocument("export default getModels();", "model-a").ok, false);
  });

  it("导入多模型文档时只匹配精确 slug", () => {
    const result = parseModelMetadataDocument(
      JSON.stringify({ models: [{ slug: "model-a", marker: "a" }, { slug: "model-b", marker: "b" }] }),
      "model-b",
    );
    assert.strictEqual(result.ok, true);
    if (result.ok) assert.deepStrictEqual(result.value.metadata, { marker: "b" });
  });

  it("替换、清除、保留和 slug 重命名只影响 metadata map", () => {
    const replaced = replaceModelMetadataForSlug(
      '{"model-a":{"old":true},"other":{"keep":true}}',
      "model-a",
      { supports_search_tool: true, priority: 2 },
    );
    assert.deepStrictEqual(JSON.parse(replaced), {
      "model-a": { supports_search_tool: true, priority: 2 },
      other: { keep: true },
    });
    assert.strictEqual(clearModelMetadataForSlug(replaced, "model-a"), '{"other":{"keep":true}}');
    assert.strictEqual(
      remapModelMetadataSlugs('{"a":{"x":1},"b":{"x":2}}', [
        { previousSlug: "a", nextSlug: "b" },
        { previousSlug: "b", nextSlug: "c" },
      ]),
      '{"b":{"x":1},"c":{"x":2}}',
    );
    assert.strictEqual(
      retainModelMetadataForSlugs('{"a":{"x":1},"deleted":{"x":2}}', ["a"]),
      '{"a":{"x":1}}',
    );
  });

  it("保留 Codex++ 已填写的显示名称，其他 metadata 采用最新导入值", () => {
    const replaced = replaceModelMetadataForSlug(
      '{"model-a":{"display_name":"我的模型名","vendor":"old"}}',
      "model-a",
      { display_name: "供应商模型名", vendor: "new", supports_search_tool: true },
    );
    assert.deepStrictEqual(JSON.parse(replaced), {
      "model-a": {
        display_name: "我的模型名",
        vendor: "new",
        supports_search_tool: true,
      },
    });
  });

  it("模型窗口和比例使用十进制 K/M 及 half-up 舍入", () => {
    const document = serializeModelMetadataDocument("model-a", { vendor: "x" }, "1M", "80%");
    assert.deepStrictEqual(JSON.parse(document), {
      models: [{ slug: "model-a", context_window: 1_000_000, auto_compact_token_limit: 800_000, vendor: "x" }],
    });
    const rounded = synchronizeModelMetadataDocumentLimits(
      '{"slug":"tiny","context_window":3}',
      "tiny",
      "3",
      "50%",
    );
    assert.strictEqual(JSON.parse(rounded ?? "null").auto_compact_token_limit, 2);
  });

  it("空比例保持 Codex 默认行为并保留字段位置", () => {
    const document = synchronizeModelMetadataDocumentLimits(
      '{"slug":"model-a","context_window":100,"auto_compact_token_limit":90}',
      "model-a",
      "200",
      "",
    );
    assert.deepStrictEqual(JSON.parse(document ?? "null"), {
      slug: "model-a",
      context_window: 200,
      auto_compact_token_limit: null,
    });
  });

  it("自动压缩清空后重新输入不改变 JSON 字段顺序", () => {
    const source = '{"slug":"model-a","context_window":100,"auto_compact_token_limit":90,"vendor":true}';
    const cleared = synchronizeModelMetadataDocumentLimits(source, "model-a", "100", "");
    assert.ok(cleared);
    const refilled = synchronizeModelMetadataDocumentLimits(cleared!, "model-a", "100", "80%");
    assert.ok(refilled);
    assert.deepStrictEqual(Object.keys(JSON.parse(refilled!)), [
      "slug",
      "context_window",
      "auto_compact_token_limit",
      "vendor",
    ]);
    assert.strictEqual(JSON.parse(refilled!).auto_compact_token_limit, 80);
  });

  it("压缩百分比保存再打开时始终把 context_window 放在前面", () => {
    const source = '{"slug":"model-a","vendor":true,"auto_compact_token_limit":90,"context_window":100}';
    const saved = synchronizeModelMetadataDocumentLimits(source, "model-a", "200", "80%");
    assert.ok(saved);
    assert.deepStrictEqual(Object.keys(JSON.parse(saved!)), [
      "slug",
      "context_window",
      "auto_compact_token_limit",
      "vendor",
    ]);
    const reopened = parseModelMetadataDocument(saved!, "model-a");
    assert.strictEqual(reopened.ok, true);
    if (reopened.ok) assert.strictEqual(reopened.value.contextWindow, "200");
  });

  it("预览在修改窗口后保留显式高精度比例", () => {
    const synchronized = synchronizeModelMetadataDocumentLimitsPreview(
      '{"slug":"model-a","context_window":272000,"auto_compact_token_limit":229376}',
      "model-a",
      "800000",
      "84.329412%",
    );
    assert.ok(synchronized);
    assert.strictEqual(synchronized?.preview.autoCompactPercent, "84%");
    assert.strictEqual(synchronized?.preview.autoCompactCalculationPercent, "84.329412%");
    assert.strictEqual(JSON.parse(synchronized?.document ?? "null").auto_compact_token_limit, 674635);
  });

  it("窗口清空时保留 context_window 字段位置", () => {
    const document = synchronizeModelMetadataDocumentContextWindow(
      '{"slug":"model-a","context_window":100,"priority":1}',
      "model-a",
      "",
    );
    assert.deepStrictEqual(JSON.parse(document ?? "null"), { slug: "model-a", context_window: null, priority: 1 });
  });

  it("context_window 为 null 时按未设置处理", () => {
    const result = parseModelMetadataDocument(
      '{"slug":"model-a","context_window":null,"vendor":true}',
      "model-a",
    );
    assert.strictEqual(result.ok, true);
    if (result.ok) assert.strictEqual(result.value.contextWindow, null);
  });

  it("前端比例校验与 Rust 语法一致", () => {
    for (const value of ["90", "84.5%", "0.000001", "100%", ""]) {
      assert.strictEqual(isValidAutoCompactPercent(value), true, value);
    }
    for (const value of ["0", "101%", "90%%", ".5", "1.1234567"]) {
      assert.strictEqual(isValidAutoCompactPercent(value), false, value);
      assert.strictEqual(normalizeAutoCompactPercent(value), value);
    }
  });

  it("坏 metadata map 在 UI 侧不抛异常", () => {
    assert.deepStrictEqual(parseModelMetadataMap("not-json"), {});
  });
});
