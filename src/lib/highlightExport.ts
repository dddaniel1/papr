import type { ArticleDetail, Highlight } from "../types";

function markdownText(text: string): string {
  return text.replace(/\r\n?/g, "\n").replace(/&/g, "&amp;").replace(/[\\`*_[\]{}<>#+.!|~-]/g, "\\$&");
}

export function highlightsMarkdown(
  article: Pick<ArticleDetail, "title" | "feedTitle" | "url">,
  highlights: readonly Highlight[],
): string {
  const parts = [
    `# ${markdownText(article.title.replace(/\s+/g, " "))}`,
    markdownText(article.feedTitle.replace(/\s+/g, " ")),
  ];
  if (article.url && /^https?:\/\//i.test(article.url)) {
    parts.push(`<${article.url.replace(/[\s<>]/g, encodeURIComponent)}>`);
  }
  for (const hl of [...highlights].sort((a, b) => a.textOffset - b.textOffset || a.id - b.id)) {
    parts.push(markdownText(hl.quote).split("\n").map((line) => `> ${line}`).join("\n"));
    if (hl.note.trim()) parts.push(markdownText(hl.note));
  }
  return parts.join("\n\n") + "\n";
}
