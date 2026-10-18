import { mockIPC } from "@tauri-apps/api/mocks";
import { normalizeImageHtml, watchImage } from "../src/lib/imageLoading";
import "../src/styles.css";

const png = Uint8Array.from(atob("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aOuoAAAAASUVORK5CYII="), (c) => c.charCodeAt(0));
let failing = false;
let calls = 0;
let finish: ((value: number[]) => void) | undefined;
let deferred = false;
mockIPC(async (cmd) => {
  if (cmd !== "fetch_image") return;
  calls++;
  if (failing) throw new Error("offline");
  if (deferred) return new Promise<number[]>((resolve) => { finish = resolve; });
  return Array.from(png);
});
const labels = { failed: "图片加载失败", retry: "重试", open: "打开原图" };
const waitFor = async (condition: () => boolean) => {
  const deadline = Date.now() + 3000;
  while (!condition()) {
    if (Date.now() > deadline) throw new Error("Image check timed out");
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
};
const assert = (value: unknown, message: string) => { if (!value) throw new Error(message); };
const root = document.getElementById("root")!;
function mount() {
  const img = document.createElement("img");
  img.src = "https://cdnfile.sspai.com/image-test";
  root.append(img);
  return img;
}
async function runChecks() {
  const checks: string[] = [];
  const html = normalizeImageHtml('<picture><source srcset="images/a.png 1x, images/b.png 2x"><img src="/images/placeholder.gif" data-src="images/real.png"></picture>', "https://site.test/post/one");
  const doc = new DOMParser().parseFromString(html, "text/html");
  assert(doc.querySelector("img")?.getAttribute("src") === "https://site.test/post/images/real.png", "Lazy address not recovered");
  checks.push("Lazy and article-relative addresses restored");

  failing = true;
  const img = mount();
  let opened = "";
  const stop = watchImage(img, "https://site.test/post", labels, (url) => { opened = url; });
  await waitFor(() => !!root.querySelector(".image-failure"));
  const buttons = root.querySelectorAll<HTMLButtonElement>("button");
  buttons[1].click();
  assert(opened === "https://cdnfile.sspai.com/image-test", "Original image address lost");
  failing = false;
  buttons[0].click();
  await waitFor(() => img.src.startsWith("data:") && img.naturalWidth > 0);
  assert(!root.querySelector(".image-failure"), "Retry left error placeholder");
  checks.push("Failed image exposes actions; manual retry displays recovered bytes");
  stop();
  img.remove();

  deferred = true;
  const old = mount();
  const other = mount();
  const before = calls;
  const stopOld = watchImage(old, "https://site.test/shared", labels, () => {});
  const stopOther = watchImage(other, "https://site.test/shared", labels, () => {});
  assert(calls === before + 1, "Duplicate in-flight request");
  const oldSrc = old.src;
  stopOld();
  old.remove();
  await waitFor(() => !!finish);
  finish!(Array.from(png));
  await waitFor(() => other.naturalWidth > 0);
  assert(old.src === oldSrc, "Disposed image was mutated");
  checks.push("Shared request deduplicated; disposed article ignores late result");
  stopOther();
  other.remove();
  deferred = false;
  root.innerHTML = '<p>图片检查通过</p>';
  failing = true;
  const failed = mount();
  watchImage(failed, null, labels, () => {});
  await waitFor(() => !!root.querySelector(".image-failure"));
  return checks;
}
Object.assign(window, { imageChecks: { runChecks } });
