import { describe, expect, it } from "vitest";
import { bilibiliPlayerUrl } from "./bilibili";

describe("bilibiliPlayerUrl", () => {
  it.each(["bilibili.com", "www.bilibili.com", "m.bilibili.com"])("supports RSSHub BV links on %s", (host) => {
    const player = new URL(bilibiliPlayerUrl(`https://${host}/video/BV1xx411c7mD/?spm_id_from=333.999`)!);
    expect(player.origin + player.pathname).toBe("https://player.bilibili.com/player.html");
    expect(Object.fromEntries(player.searchParams)).toEqual({ bvid: "BV1xx411c7mD", autoplay: "0" });
  });

  it("supports legacy av links and preserves the selected part", () => {
    expect(bilibiliPlayerUrl("http://www.bilibili.com/video/av170001?p=2&autoplay=1#reply"))
      .toBe("https://player.bilibili.com/player.html?aid=170001&p=2&autoplay=0");
  });

  it.each(["0", "-1", "1.5", "NaN", "2&autoplay=1"])("ignores invalid part %s", (page) => {
    const player = new URL(bilibiliPlayerUrl(`https://www.bilibili.com/video/av170001?p=${encodeURIComponent(page)}`)!);
    expect(player.searchParams.has("p")).toBe(false);
    expect(player.searchParams.get("autoplay")).toBe("0");
  });

  it.each([
    null, "", "invalid", "javascript:alert(1)",
    "https://www.bilibili.com.evil.test/video/av170001",
    "https://evil.test/video/BV1xx411c7mD",
    "https://www.bilibili.com@evil.test/video/av170001",
    "https://user:pass@www.bilibili.com/video/av170001",
    "https://www.bilibili.com:1234/video/av170001",
    "ftp://www.bilibili.com/video/av170001",
    "https://www.bilibili.com/video/BVinvalid",
    "https://www.bilibili.com/video/av0",
    "https://www.bilibili.com/video/av170001/extra",
    "https://space.bilibili.com/2267573", "https://t.bilibili.com/12345",
  ])("does not embed unsupported URL %s", (url) => {
    expect(bilibiliPlayerUrl(url)).toBeNull();
  });
});
