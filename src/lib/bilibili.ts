/** Build an official player URL without passing feed-controlled embed URLs through. */
export function bilibiliPlayerUrl(articleUrl: string | null): string | null {
  if (!articleUrl) return null;
  let url: URL;
  try {
    url = new URL(articleUrl);
  } catch {
    return null;
  }
  if (
    !["http:", "https:"].includes(url.protocol) ||
    !["bilibili.com", "www.bilibili.com", "m.bilibili.com"].includes(url.hostname) ||
    url.username || url.password || url.port
  ) return null;

  const id = url.pathname.match(/^\/video\/(BV[0-9A-Za-z]{10}|av[1-9]\d*)\/?$/)?.[1];
  if (!id) return null;
  const player = new URL("https://player.bilibili.com/player.html");
  player.searchParams.set(id.startsWith("BV") ? "bvid" : "aid", id.startsWith("BV") ? id : id.slice(2));
  const page = url.searchParams.get("p");
  if (page && /^[1-9]\d*$/.test(page)) player.searchParams.set("p", page);
  player.searchParams.set("autoplay", "0");
  return player.href;
}
