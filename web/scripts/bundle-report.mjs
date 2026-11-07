#!/usr/bin/env node
// What the production bundle is made of: builds with source maps into a
// scratch directory (dist/ is left alone), then lists every emitted file with
// its raw and gzipped size, and attributes each JavaScript chunk's bytes to
// the modules its source map names (grouped by package or by src/ folder).
//
//   node scripts/bundle-report.mjs                 # build, then report
//   node scripts/bundle-report.mjs --dir DIR       # report on an existing build made with --sourcemap
//   node scripts/bundle-report.mjs --json FILE     # also write the numbers as JSON
//   node scripts/bundle-report.mjs --top 25        # rows per chunk (default 15)

import { mkdtempSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { gzipSync } from 'node:zlib';
import { eachMapping, TraceMap } from '@jridgewell/trace-mapping';

const webDir = fileURLToPath(new URL('..', import.meta.url));
const argv = process.argv.slice(2);
const opt = (name) => {
  const i = argv.indexOf(name);
  return i >= 0 ? argv[i + 1] : undefined;
};
const top = Number(opt('--top') ?? 15);
let dir = opt('--dir');
let scratch = null;

if (!dir) {
  scratch = mkdtempSync(join(tmpdir(), 'wx-bundle-'));
  dir = scratch;
  const { build } = await import('vite');
  await build({
    root: webDir,
    configFile: join(webDir, 'vite.config.ts'),
    logLevel: 'warn',
    build: { outDir: dir, emptyOutDir: true, sourcemap: true },
  });
}

const kb = (n) => `${(n / 1024).toFixed(1)} KB`;
const gz = (buf) => gzipSync(buf, { level: 9 }).length;

/** Every file under `root`, recursively. */
function files(root, sub = '') {
  return readdirSync(join(root, sub)).flatMap((name) => {
    const rel = join(sub, name);
    return statSync(join(root, rel)).isDirectory() ? files(root, rel) : [rel];
  });
}

/** Group a source path by package (node_modules/<pkg>) or src/ folder. */
function groupOf(source) {
  const s = source.replace(/\\/g, '/').replace(/^(\.\.\/)+/, '');
  const nm = s.lastIndexOf('node_modules/');
  if (nm >= 0) {
    const rest = s.slice(nm + 'node_modules/'.length).split('/');
    return rest[0].startsWith('@') ? `${rest[0]}/${rest[1]}` : rest[0];
  }
  const m = s.match(/(?:^|\/)web\/(src\/[^/]+|mock|[^/]+)/) ?? s.match(/^(src\/[^/]+)/);
  return m ? m[1] : s;
}

/** Bytes of generated code attributed to each source of one chunk. */
function attribute(code, map) {
  const tm = new TraceMap(map);
  const lines = code.split('\n');
  const lineStart = [];
  let off = 0;
  for (const l of lines) {
    lineStart.push(off);
    off += Buffer.byteLength(l) + 1;
  }
  const segs = [];
  eachMapping(tm, (m) => segs.push({ line: m.generatedLine - 1, col: m.generatedColumn, source: m.source ?? '(none)' }));
  segs.sort((a, b) => a.line - b.line || a.col - b.col);
  const bySource = new Map();
  const add = (src, text) => {
    const e = bySource.get(src) ?? { bytes: 0, parts: [] };
    e.bytes += Buffer.byteLength(text);
    e.parts.push(text);
    bySource.set(src, e);
  };
  for (let i = 0; i < segs.length; i++) {
    const s = segs[i];
    const n = segs[i + 1];
    const line = lines[s.line] ?? '';
    // Columns are UTF-16 offsets within the line.
    const end = n && n.line === s.line ? n.col : line.length;
    add(s.source, line.slice(s.col, end));
  }
  const groups = new Map();
  for (const [src, e] of bySource) {
    const g = groupOf(src);
    const cur = groups.get(g) ?? { bytes: 0, text: [] };
    cur.bytes += e.bytes;
    for (const t of e.parts) cur.text.push(t);
    groups.set(g, cur);
  }
  return [...groups.entries()]
    .map(([group, e]) => ({ group, bytes: e.bytes, gzip: gz(Buffer.from(e.text.join(''))) }))
    .sort((a, b) => b.bytes - a.bytes);
}

const report = { files: [], chunks: {} };
const all = files(dir).filter((f) => !f.endsWith('.map'));
const assets = all.filter((f) => f.startsWith('assets/'));
const other = all.filter((f) => !f.startsWith('assets/'));
console.log(`Build in ${dir}\n`);
console.log('Emitted assets (raw, gzip -9):');
for (const f of assets.sort()) {
  const buf = readFileSync(join(dir, f));
  const row = { file: f, bytes: buf.length, gzip: gz(buf) };
  const map = join(dir, `${f}.map`);
  try {
    row.mapBytes = statSync(map).size;
  } catch {
    // No source map for this one.
  }
  report.files.push(row);
  console.log(`  ${f.padEnd(52)} ${kb(row.bytes).padStart(11)} ${kb(row.gzip).padStart(11)}${row.mapBytes ? `   (map ${kb(row.mapBytes)})` : ''}`);
}
const sum = (rows) => rows.reduce((n, r) => n + r, 0);
const otherBytes = sum(other.map((f) => statSync(join(dir, f)).size));
console.log(`  (plus ${other.length} files outside assets/, ${kb(otherBytes)}: public/ copied as is)\n`);

for (const f of assets.filter((x) => /\.(m?js)$/.test(x))) {
  let map;
  try {
    map = JSON.parse(readFileSync(join(dir, `${f}.map`), 'utf8'));
  } catch {
    continue;
  }
  const code = readFileSync(join(dir, f), 'utf8');
  const rows = attribute(code, map);
  report.chunks[f] = rows;
  if (Buffer.byteLength(code) < 20_000) continue;
  console.log(`${f}: ${kb(Buffer.byteLength(code))}, by origin (gzip of each group alone):`);
  for (const r of rows.slice(0, top)) console.log(`  ${r.group.padEnd(40)} ${kb(r.bytes).padStart(11)} ${kb(r.gzip).padStart(11)}`);
  if (rows.length > top) console.log(`  … ${rows.length - top} more, ${kb(sum(rows.slice(top).map((r) => r.bytes)))}`);
  console.log('');
}

const jsonOut = opt('--json');
if (jsonOut) writeFileSync(jsonOut, JSON.stringify(report, null, 2));
if (scratch) rmSync(scratch, { recursive: true, force: true });
