// Generates a 44x44 monochrome (black + transparent) tray icon for macOS.
// This is a "template image" — black pixels with transparency that the system
// automatically adapts to light/dark mode when icon_as_template(true) is set.
// 44x44 px = 22x22 points @2x (Retina), the standard macOS status bar icon size.
import { deflateSync } from "node:zlib";
import { writeFileSync } from "node:fs";

const SIZE = 44;
const STROKE = 4;

// W stroke path scaled from the 1024px app icon to 44px.
// Occupies ~77% of the canvas with proper margins.
const SEGMENTS = [
  [7, 8, 15, 38],
  [15, 38, 22, 17],
  [22, 17, 29, 38],
  [29, 38, 37, 8],
];

function distanceToSegment(px, py, [x1, y1, x2, y2]) {
  const dx = x2 - x1;
  const dy = y2 - y1;
  const lengthSq = dx * dx + dy * dy;
  const t = Math.max(0, Math.min(1, ((px - x1) * dx + (py - y1) * dy) / lengthSq));
  const cx = x1 + t * dx;
  const cy = y1 + t * dy;
  return Math.hypot(px - cx, py - cy);
}

const raw = Buffer.alloc((SIZE * 4 + 1) * SIZE);

for (let y = 0; y < SIZE; y++) {
  const rowStart = y * (SIZE * 4 + 1);
  raw[rowStart] = 0; // PNG filter type: none

  for (let x = 0; x < SIZE; x++) {
    const px = x + 0.5;
    const py = y + 0.5;

    const glyphDistance = Math.min(
      ...SEGMENTS.map((segment) => distanceToSegment(px, py, segment)),
    );
    // 1-pixel anti-aliasing band
    const glyphAlpha = Math.min(1, Math.max(0, STROKE / 2 + 0.5 - glyphDistance));

    const offset = rowStart + 1 + x * 4;
    raw[offset] = 0; // R
    raw[offset + 1] = 0; // G
    raw[offset + 2] = 0; // B
    raw[offset + 3] = Math.round(glyphAlpha * 255); // A
  }
}

function chunk(type, data) {
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length);

  const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body) >>> 0);

  return Buffer.concat([length, body, crc]);
}

const CRC_TABLE = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});

function crc32(buffer) {
  let c = 0xffffffff;
  for (const byte of buffer) c = CRC_TABLE[(c ^ byte) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(SIZE, 0);
ihdr.writeUInt32BE(SIZE, 4);
ihdr[8] = 8; // bit depth
ihdr[9] = 6; // colour type: RGBA

const png = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  chunk("IHDR", ihdr),
  chunk("IDAT", deflateSync(raw, { level: 9 })),
  chunk("IEND", Buffer.alloc(0)),
]);

writeFileSync(new URL("../src-tauri/icons/tray-icon.png", import.meta.url), png);
console.log(`wrote tray-icon.png (${SIZE}x${SIZE}, ${png.length} bytes)`);
