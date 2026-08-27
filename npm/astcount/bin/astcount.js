#!/usr/bin/env node

import { existsSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const require = createRequire(import.meta.url);
const platforms = {
  "darwin-arm64": {
    packageName: "astcount-darwin-arm64",
    directory: "darwin-arm64"
  },
  "darwin-x64": {
    packageName: "astcount-darwin-x64",
    directory: "darwin-x64"
  },
  "linux-arm64": {
    packageName: "astcount-linux-arm64-gnu",
    directory: "linux-arm64-gnu"
  },
  "linux-x64": {
    packageName: "astcount-linux-x64-gnu",
    directory: "linux-x64-gnu"
  }
};

const key = `${process.platform}-${process.arch}`;
const platform = platforms[key];
if (!platform) {
  fail(`unsupported platform ${key}`);
}
if (process.platform === "linux" && !isGlibc()) {
  fail("Linux musl is not supported yet; use the Nix package or build from source");
}

let binary;
try {
  const manifest = require.resolve(`${platform.packageName}/package.json`);
  binary = `${dirname(manifest)}/bin/astcount`;
} catch {
  const developmentBinary = fileURLToPath(
    new URL(`../../platforms/${platform.directory}/bin/astcount`, import.meta.url)
  );
  if (existsSync(developmentBinary)) {
    binary = developmentBinary;
  } else {
    fail(
      `${platform.packageName} is missing; reinstall without omitting optional dependencies`
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
