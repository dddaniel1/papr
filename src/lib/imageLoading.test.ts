import { beforeEach, describe, expect, it, vi } from "vitest";
vi.mock("../api", () => ({ fetchImage: vi.fn() }));
import { fetchImage } from "../api";
import { imageUrl, needsImageProxy, recoverImage } from "./imageLoading";

beforeEach(() => vi.mocked(fetchImage).mockReset());

describe("image recovery", () => {
  it("shares in-flight requests only for the same URL and page context", async () => {
    let finish!: (bytes: Uint8Array) => void;
    vi.mocked(fetchImage).mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
    const first = recoverImage("https://img.test/x", "https://page.test/a");
    expect(recoverImage("https://img.test/x", "https://page.test/a")).toBe(first);
    expect(fetchImage).toHaveBeenCalledTimes(1);
    finish(new Uint8Array([0xff, 0xd8, 0xff]));
    expect(await first).toContain("data:image/jpeg;base64,");
    vi.mocked(fetchImage).mockResolvedValue(new Uint8Array([0xff, 0xd8, 0xff]));
    await recoverImage("https://img.test/x", "https://page.test/a");
    await recoverImage("https://img.test/x", "https://page.test/b");
    expect(fetchImage).toHaveBeenCalledTimes(3);
  });
  it("does not cache failures, so manual retry can recover", async () => {
    vi.mocked(fetchImage).mockRejectedValueOnce(new Error("offline"));
    await expect(recoverImage("https://img.test/retry")).rejects.toThrow("offline");
    vi.mocked(fetchImage).mockResolvedValue(new Uint8Array([0xff, 0xd8, 0xff]));
    await expect(recoverImage("https://img.test/retry")).resolves.toContain("data:image/jpeg");
  });
});

it("resolves article-relative paths and excludes executable/local schemes", () => {
  expect(imageUrl("images/a.jpg", "https://site.test/posts/one")).toBe("https://site.test/posts/images/a.jpg");
  expect(imageUrl("//cdn.test/a", "https://site.test/post")).toBe("https://cdn.test/a");
  expect(imageUrl("", "https://site.test/post")).toBeNull();
  expect(imageUrl("javascript:alert(1)")).toBeNull();
  expect(imageUrl("file:///etc/passwd")).toBeNull();
  expect(needsImageProxy("https://cdnfile.sspai.com/a")).toBe(true);
  expect(needsImageProxy("https://cdnfile.sspai.com.evil.test/a")).toBe(false);
  expect(needsImageProxy("https://example.com/photo.heic")).toBe(true);
  expect(needsImageProxy("https://example.com/photo.heif?w=800")).toBe(true);
  expect(needsImageProxy("https://example.com/img?src=foo.heic")).toBe(true);
  expect(needsImageProxy("https://example.com/photo.jpg")).toBe(false);
});
