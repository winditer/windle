// Type-checks the Rust backend for the Windows target from a non-Windows host.
//
// `tauri-build` compiles a Windows resource file through `llvm-rc` (or `rc.exe`
// on Windows). Neither tool ships with Xcode, so this script drops a no-op
// `llvm-rc` on the PATH first: resource compilation only affects the final
// binary's icon/manifest, while the type checking this is meant for runs
// unchanged. Real resource compilation happens on Windows — see
// .github/workflows/windows-build.yml and `npm run tauri build`.
//
// Usage: node scripts/check-windows.mjs [extra cargo args]
import { chmodSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";

const SHIM = `#!/bin/sh
# No-op resource compiler: create the requested output and report success.
out=""
while [ $# -gt 0 ]; do
  case "$1" in
    /fo|/FO) shift; out="$1" ;;
    /fo:*|/FO:*|/out:*) out="\${1#*:}" ;;
  esac
  shift
done
[ -n "$out" ] && : > "$out" 2>/dev/null
exit 0
`;

const dir = mkdtempSync(join(tmpdir(), "windle-rc-"));
const shim = join(dir, "llvm-rc");
writeFileSync(shim, SHIM);
chmodSync(shim, 0o755);

const result = spawnSync(
  "cargo",
  [
    "check",
    "--target",
    "x86_64-pc-windows-msvc",
    "--all-targets",
    ...process.argv.slice(2),
  ],
  {
    cwd: new URL("../src-tauri", import.meta.url).pathname,
    stdio: "inherit",
    env: { ...process.env, PATH: `${dir}:${process.env.PATH}` },
  },
);

process.exit(result.status ?? 1);
