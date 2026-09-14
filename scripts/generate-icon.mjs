// One-off generator for app-icon.png — the source image `tauri icon` expands
// into every bundle size. Draws a rounded green tile with a white "W".
import { deflateSync } from "node:zlib";
import { writeFileSync } from "node:fs";

const SIZE = 1024;
const RADIUS = 224;
const STROKE = 72;

// Stroke path for the W, as a list of segments.
// Scaled to occupy ~80% of the canvas with proper margins (~10% per side),
// per macOS Big Sur icon guidelines for content within the squircle.
const SEGMENTS = [
  [171, 183, 348, 888],
  [348, 888, 512, 406],
  [512, 406, 676, 888],
  [676, 888, 853, 183],
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

// Signed distance to the rounded-rect boundary, negative inside.
function roundedRectDistance(px, py) {
  const half = SIZE / 2;
  const qx = Math.abs(px - half) - (half - RADIUS);
  const qy = Math.abs(py - half) - (half - RADIUS);
  const outside = Math.hypot(Math.max(qx, 0), Math.max(qy, 0));
  return outside + Math.min(Math.max(qx, qy), 0) - RADIUS;
}

function mix(a, b, t) {
  return Math.round(a + (b - a) * t);
}

const raw = Buffer.alloc((SIZE * 4 + 1) * SIZE);

for (let y = 0; y < SIZE; y++) {
  const rowStart = y * (SIZE * 4 + 1);
  raw[rowStart] = 0; // PNG filter type: none

  for (let x = 0; x < SIZE; x++) {
    const px = x + 0.5;
    const py = y + 0.5;

    // Antialias both shapes over a one pixel band.
    const tileAlpha = Math.min(1, Math.max(0, 0.5 - roundedRectDistance(px, py)));
    const glyphDistance = Math.min(
      ...SEGMENTS.map((segment) => distanceToSegment(px, py, segment)),
    );
    const glyphAlpha = Math.min(1, Math.max(0, STROKE / 2 + 0.5 - glyphDistance));

    const gradient = y / SIZE;
    // Green gradient matching the app theme (HSL 142 71% 45% ≈ #21C45D).
    const r = mix(33, 19, gradient);
    const g = mix(196, 138, gradient);
    const b = mix(93, 62, gradient);

    const offset = rowStart + 1 + x * 4;
    raw[offset] = mix(r, 255, glyphAlpha);
    raw[offset + 1] = mix(g, 255, glyphAlpha);
    raw[offset + 2] = mix(b, 255, glyphAlpha);
    raw[offset + 3] = Math.round(tileAlpha * 255);
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

writeFileSync(new URL("../app-icon.png", import.meta.url), png);
console.log(`wrote app-icon.png (${SIZE}x${SIZE}, ${png.length} bytes)`);
