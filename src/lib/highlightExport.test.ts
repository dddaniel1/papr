import { describe, expect, it } from "vitest";
import { marked } from "marked";
import type { Highlight } from "../types";
import { highlightsMarkdown } from "./highlightExport";

const article = { title: "阅读笔记", feedTitle: "Test feed", url: "https://example.com/article" };
const highlight = (id: number, textOffset: number, quote: string, note = ""): Highlight => ({
  id, articleId: 1, textOffset, quote, note, prefix: "", suffix: "", color: "yellow", createdAt: "",
});

describe("highlightsMarkdown", () => {
  it("includes article provenance and exports full quotes and notes in reading order without mutating input", () => {
    const highlights = [highlight(2, 20, "Second quote"), highlight(1, 0, "第一行\r\n\r\n第二行", "我的笔记\n下一行")];
    expect(highlightsMarkdown(article, highlights)).toBe(
      "# 阅读笔记\n\nTest feed\n\n<https://example.com/article>\n\n> 第一行\n> \n> 第二行\n\n我的笔记\n下一行\n\n> Second quote\n",
    );
    expect(highlights.map((h) => h.id)).toEqual([2, 1]);
  });

  it("keeps Markdown and HTML in source text literal", () => {
    const markdown = highlightsMarkdown(
      { ...article, title: "[Title]\n# Heading", feedTitle: "*Feed*", url: "https://example.com/a<b> c" },
      [highlight(1, 0, "**bold** [link](javascript:alert(1)) <script>x</script> &lt;b&gt; &amp;", "# Note\n- item\n![image](https://example.com/image)")],
    );
    const html = marked.parse(markdown) as string;
    expect(html).not.toMatch(/<script|<img|<strong|<em>|<ul>|<h1>Note/);
    expect(html).toContain("https://example.com/a%3Cb%3E%20c");
    expect(html).toContain("[link](javascript:alert(1))");
    expect(html).toContain("&amp;lt;b&amp;gt; &amp;amp;");
    expect(html.match(/<h1>/g)).toHaveLength(1);
  });

  it("omits missing or unsafe source links and blank notes", () => {
    for (const url of [null, "javascript:alert(1)"]) {
      expect(highlightsMarkdown({ ...article, url }, [highlight(1, 0, "Quote", "  ")])).toBe(
        "# 阅读笔记\n\nTest feed\n\n> Quote\n",
      );
    }
  });
});
