#!/usr/bin/env node

import { existsSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const require = createRequire(import.meta.url);
const key = `${process.platform}-${process.arch}`;
const supported = new Set(["darwin-arm64", "darwin-x64", "linux-arm64", "linux-x64"]);
if (!supported.has(key)) {
  fail(`unsupported platform ${key}`);
}
if (process.platform === "linux" && !isGlibc()) {
  fail("Linux musl is not supported yet; use the Nix package or build from source");
}
const target = process.platform === "linux" ? `${key}-gnu` : key;
const packageName = `astcount-${target}`;

let binary;
try {
  const manifest = require.resolve(`${packageName}/package.json`);
  binary = `${dirname(manifest)}/bin/astcount`;
} catch {
  const developmentBinary = fileURLToPath(
    new URL(`../../platforms/${target}/bin/astcount`, import.meta.url)
  );
  if (existsSync(developmentBinary)) {
    binary = developmentBinary;
  } else {
    fail(
      `${packageName} is missing; reinstall without omitting optional dependencies`
    );
  }
}

const result = spawnSync(binary, process.argv.slice(2), { stdio: "inherit" });
if (result.error) {
  fail(`could not execute ${binary}: ${result.error.message}`);
}
if (result.signal) {
  process.kill(process.pid, result.signal);
} else {
  process.exit(result.status ?? 1);
}

function isGlibc() {
  return Boolean(process.report?.getReport()?.header?.glibcVersionRuntime);
}

function fail(message) {
  console.error(`astcount: ${message}`);
  process.exit(1);
}
