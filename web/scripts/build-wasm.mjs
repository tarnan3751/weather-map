#!/usr/bin/env node
// Builds crates/wx-tile-wasm for wasm32-unknown-unknown and copies the module
// to public/wasm/, where the decode worker looks for it. Optional: without it
// the client decodes tiles in TypeScript.
//
// The `wasm-release` profile in the workspace's Cargo.toml tunes it for size
// and speed (opt-level 3 rather than "z": the module is only ~1% larger, about
// 55 KB or 24 KB gzipped, and decodes roughly a third faster).

import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync, readFileSync, statSync } from 'node:fs';
import { gzipSync } from 'node:zlib';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../..', import.meta.url));
const out = fileURLToPath(new URL('../public/wasm/', import.meta.url));
const args = ['build', '--profile', 'wasm-release', '--target', 'wasm32-unknown-unknown', '-p', 'wx-tile-wasm'];
execFileSync('cargo', args, { cwd: root, stdio: 'inherit' });

const built = `${root}target/wasm32-unknown-unknown/wasm-release/wx_tile_wasm.wasm`;
mkdirSync(out, { recursive: true });
copyFileSync(built, `${out}wx_tile_wasm.wasm`);
const raw = statSync(built).size;
const gz = gzipSync(readFileSync(built), { level: 9 }).length;
console.log(`public/wasm/wx_tile_wasm.wasm: ${raw} bytes, ${gz} gzipped`);
