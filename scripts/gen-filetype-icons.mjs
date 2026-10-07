// 生成文件类型图标：
// - Windows：对每个 SVG 源调用 `tauri icon` 光栅化为多尺寸 PNG，再封装为 ICO（PNG 压缩，Vista+）。
//   尺寸集 16/32/48/256 覆盖资源管理器各视图与任务栏。
// - macOS：额外产出 .icns（iconutil 打包 iconset），供 Info.plist 的 CFBundleTypeIconFile 引用，
//   解决 .mdl/.mdlb 在 Finder 里回退为「通用文档 + App 图标徽标」的问题。
// 每个图标由两档图源合成：小尺寸帧（16/32）用加粗高对比的 *-sm.svg，大尺寸帧用精细 .svg。
//
// 用法：node scripts/gen-filetype-icons.mjs
import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync, mkdtempSync, mkdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
// Windows 下 npx 是 npx.cmd 批处理，spawnSync 不开 shell 解析不到，按平台选入口。
const NPX = process.platform === "win32" ? "npx.cmd" : "npx";
const OUT_DIR = join(ROOT, "src-tauri", "icons", "filetypes");
const SM_SIZES = [16, 32];
const LG_SIZES = [48, 256];
// macOS .icns 需要的尺寸集（含 @2x 派生），小尺寸仍用 *-sm.svg 保证列表视图清晰。
const ICNS_SM_SIZES = [16, 32];
const ICNS_LG_SIZES = [64, 128, 256, 512, 1024];

// 源 SVG → 输出图标基础名。
const ICONS = [
  { base: "md",   sm: "src-tauri/icons/filetypes/filetype-md-sm.svg",   lg: "src-tauri/icons/filetypes/filetype-md.svg" },
  { base: "mdl",  sm: "src-tauri/icons/filetypes/filetype-mdl-sm.svg",  lg: "src-tauri/icons/filetypes/filetype-mdl.svg" },
  { base: "mdlb", sm: "src-tauri/icons/filetypes/filetype-mdlb-sm.svg", lg: "src-tauri/icons/filetypes/filetype-mdlb.svg" },
];

// 把若干 PNG 缓冲封装为单个 ICO 二进制。
function buildIco(pngEntries) {
  const count = pngEntries.length;
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0); // reserved
  header.writeUInt16LE(1, 2); // type = icon
  header.writeUInt16LE(count, 4);

  const nameLen = 6 + count * 16;
  let dataOffset = nameLen;
  const dir = Buffer.alloc(count * 16);
  const blobs = [];

  pngEntries.forEach((png, i) => {
    const size = png.__size; // 逻辑像素边长
    const base = i * 16;
    // 256 在单字节字段里记 0；宽高仅对 ≤255 有意义。
    dir.writeUInt8(size >= 256 ? 0 : size, base + 0);
    dir.writeUInt8(size >= 256 ? 0 : size, base + 1);
    dir.writeUInt8(0, base + 2); // color count（32 位色省略）
    dir.writeUInt8(0, base + 3); // reserved
    dir.writeUInt16LE(1, base + 4); // planes
    dir.writeUInt16LE(32, base + 6); // bit count
    dir.writeUInt32LE(png.length, base + 8); // bytes in resource
    dir.writeUInt32LE(dataOffset, base + 12); // image offset
    dataOffset += png.length;
    blobs.push(png);
  });

  return Buffer.concat([header, dir, ...blobs]);
}

// 用 tauri icon 把单个 SVG 光栅化为指定尺寸集的 PNG，返回带 __size 标记的缓冲数组。
function raster(svg, sizes) {
  const tmp = mkdtempSync(join(tmpdir(), "fti-"));
  try {
    execFileSync(NPX, ["tauri", "icon", svg, "-p", sizes.join(","), "-o", tmp], {
      cwd: ROOT,
      stdio: "inherit",
    });
    return sizes.map((s) => {
      const buf = readFileSync(join(tmp, `${s}x${s}.png`));
      buf.__size = s;
      return buf;
    });
  } finally {
    rmSync(tmp, { recursive: true, force: true });
  }
}

for (const { base, sm, lg } of ICONS) {
  const entries = [...raster(sm, SM_SIZES), ...raster(lg, LG_SIZES)];
  writeFileSync(join(OUT_DIR, `${base}.ico`), buildIco(entries));
  console.log(`✓ ${base}.ico ← ${sm}(${SM_SIZES.join(",")}) + ${lg}(${LG_SIZES.join(",")})`);
}

/**
 * macOS 专属：把 SVG 源打包为 .icns。
 * iconutil 需要的 iconset 目录有严格命名（icon_<W>x<H>[@2x].png），
 * 且同一物理像素尺寸可能同时充当 base 与 @2x 两档（如 32 既是 icon_32x32 又是 icon_16x16@2x），
 * 因此 raster 每张 PNG 会按映射表复制到多个目标文件名。
 */
const ICNS_MAP = [
  // [像素尺寸, iconset 文件名列表]
  [16,   ["icon_16x16.png"]],
  [32,   ["icon_16x16@2x.png", "icon_32x32.png"]],
  [64,   ["icon_32x32@2x.png"]],
  [128,  ["icon_128x128.png"]],
  [256,  ["icon_128x128@2x.png", "icon_256x256.png"]],
  [512,  ["icon_256x256@2x.png", "icon_512x512.png"]],
  [1024, ["icon_512x512@2x.png"]],
];

if (process.platform === "darwin") {
  for (const { base, sm, lg } of ICONS) {
    const iconset = mkdtempSync(join(tmpdir(), `icns-${base}-`)) + `.iconset`;
    mkdirSync(iconset);
    try {
      const pngs = [
        ...raster(sm, ICNS_SM_SIZES),
        ...raster(lg, ICNS_LG_SIZES),
      ];
      for (const png of pngs) {
        const entry = ICNS_MAP.find(([s]) => s === png.__size);
        if (!entry) continue;
        for (const name of entry[1]) {
          writeFileSync(join(iconset, name), png);
        }
      }
      const out = join(OUT_DIR, `${base}.icns`);
      execFileSync("iconutil", ["-c", "icns", iconset, "-o", out], { stdio: "inherit" });
      console.log(`✓ ${base}.icns ← iconutil (${pngs.map((p) => p.__size).join("|")} px)`);
    } finally {
      rmSync(iconset, { recursive: true, force: true });
    }
  }
} else {
  console.log("· 跳过 .icns 生成（仅 macOS 需要）");
}
