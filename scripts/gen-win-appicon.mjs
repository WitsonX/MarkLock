// 生成 Windows 专用应用图标 icon.ico（与 mac 同设计、但去除外圈留白以整体放大）。
// mac 的 app-icon.svg 带外圈留白，在 Windows 任务栏/exe 下观感偏小；
// app-icon-win.svg 采用同一设计（浅色底 + 盾牌M）但收紧 viewBox 放大，与 mac 保持视觉一致；
// 与 gen-filetype-icons.mjs 同理：tauri icon 光栅化 SVG → 多尺寸 PNG → 手写 ICO 封装。
// 注意：必须在 `tauri icon src-tauri/icons/app-icon.svg` 之后运行，覆盖它产出的 icon.ico；
//       不触碰 icon.icns / *.png（mac 沿用带留白的浅色 app-icon.svg）。
//
// 用法：node scripts/gen-win-appicon.mjs
import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
// Windows 下 npx 是 npx.cmd 批处理，spawnSync 不开 shell 解析不到，按平台选入口。
const NPX = process.platform === "win32" ? "npx.cmd" : "npx";
const SRC = "src-tauri/icons/app-icon-win.svg";
const OUT = join(ROOT, "src-tauri", "icons", "icon.ico");
// Windows 图标标准尺寸集（含高 DPI 与任务栏/小图标各档）。
const SIZES = [16, 24, 32, 48, 64, 128, 256];

function buildIco(pngEntries) {
  const count = pngEntries.length;
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(count, 4);
  let dataOffset = 6 + count * 16;
  const dir = Buffer.alloc(count * 16);
  const blobs = [];
  pngEntries.forEach((png, i) => {
    const size = png.__size;
    const base = i * 16;
    dir.writeUInt8(size >= 256 ? 0 : size, base + 0);
    dir.writeUInt8(size >= 256 ? 0 : size, base + 1);
    dir.writeUInt8(0, base + 2);
    dir.writeUInt8(0, base + 3);
    dir.writeUInt16LE(1, base + 4);
    dir.writeUInt16LE(32, base + 6);
    dir.writeUInt32LE(png.length, base + 8);
    dir.writeUInt32LE(dataOffset, base + 12);
    dataOffset += png.length;
    blobs.push(png);
  });
  return Buffer.concat([header, dir, ...blobs]);
}

const tmp = mkdtempSync(join(tmpdir(), "winapp-"));
try {
  execFileSync(NPX, ["tauri", "icon", SRC, "-p", SIZES.join(","), "-o", tmp], {
    cwd: ROOT,
    stdio: "inherit",
  });
  const all = SIZES.map((s) => {
    const buf = readFileSync(join(tmp, `${s}x${s}.png`));
    buf.__size = s;
    return buf;
  });
  // Tauri codegen 的运行时窗口图标（任务栏/标题栏/Alt-Tab）只读 ICO 第一帧
  // （tauri-codegen image.rs new_ico 取 entries()[0]）。选 48px 作首帧：任务栏 24/32、
  // Alt-Tab 48 都能干净降采样——比 256→24 的重采样更锐利（细线不再糊成一团），
  // 也不会像旧版 16px 首帧那样被拉伸发糊。Windows 资源管理器的 exe 文件图标按尺寸
  // 自选最合适的帧、与条目顺序无关，故大/小视图不受影响。
  const FIRST = 48;
  const pngEntries = [
    ...all.filter((p) => p.__size === FIRST),
    ...all.filter((p) => p.__size !== FIRST).sort((a, b) => b.__size - a.__size),
  ];
  writeFileSync(OUT, buildIco(pngEntries));
  console.log(`✓ icon.ico (Windows, 与 mac 同设计的放大版) ← ${SRC}`);
} finally {
  rmSync(tmp, { recursive: true, force: true });
}
