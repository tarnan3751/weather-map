#!/usr/bin/env node
// Renders the PWA icons in public/icons from public/icons/icon.svg, using
// headless Chromium so no image tooling is needed. Run after editing the SVG.

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';

const dir = fileURLToPath(new URL('../public/icons/', import.meta.url));
const svg = readFileSync(`${dir}icon.svg`, 'utf8');
const dataUrl = `data:image/svg+xml;base64,${Buffer.from(svg).toString('base64')}`;

// The badge is a monochrome silhouette: Android draws only its alpha.
const badge = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 96 96">
  <path d="M18 56a15 15 0 0 1 14-15 20 20 0 0 1 38 7 11 11 0 0 1-2 22H28a10 10 0 0 1-10-14Z" fill="#fff"/>
  <path d="M32 78l-3 8M48 78l-3 8M64 78l-3 8" stroke="#fff" stroke-width="5" stroke-linecap="round"/>
</svg>`;

const jobs = [
  { file: 'icon-192.png', size: 192, src: dataUrl, pad: 0, bg: 'transparent' },
  { file: 'icon-512.png', size: 512, src: dataUrl, pad: 0, bg: 'transparent' },
  // Maskable icons are cropped to a circle or squircle: keep the art inside
  // the central 80% safe zone on a full-bleed background.
  { file: 'icon-maskable-512.png', size: 512, src: dataUrl, pad: 0.12, bg: '#0b0e13' },
  { file: 'apple-touch-icon.png', size: 180, src: dataUrl, pad: 0.04, bg: '#0b0e13' },
  { file: 'badge-72.png', size: 72, src: `data:image/svg+xml;base64,${Buffer.from(badge).toString('base64')}`, pad: 0, bg: 'transparent' },
];

const browser = await chromium.launch();
const page = await browser.newPage();
for (const j of jobs) {
  await page.setViewportSize({ width: j.size, height: j.size });
  const inset = Math.round(j.size * j.pad);
  await page.setContent(
    `<html><body style="margin:0;background:${j.bg}"><img src="${j.src}" style="display:block;width:${j.size - 2 * inset}px;height:${j.size - 2 * inset}px;margin:${inset}px"></body></html>`,
  );
  await page.waitForFunction(() => document.images[0]?.complete);
  await page.screenshot({ path: `${dir}${j.file}`, omitBackground: j.bg === 'transparent' });
  console.log(`wrote ${j.file}`);
}
await browser.close();
