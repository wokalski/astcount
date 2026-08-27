#!/usr/bin/env node

import { chmod, copyFile, mkdir, readFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const platforms = [
  "darwin-arm64",
  "darwin-x64",
  "linux-arm64-gnu",
  "linux-x64-gnu"
];
const [command, ...args] = process.argv.slice(2);

if (command === "check") {
  await checkVersions(args[0]);
} else if (command === "stage") {
  const [platform, source] = args;
  if (!platforms.includes(platform) || !source) {
    fail("usage: release.mjs stage <platform> <binary>");
  }
  await checkVersions();
  const destination = join(root, "npm", "platforms", platform, "bin", "astcount");
  await mkdir(dirname(destination), { recursive: true });
  await copyFile(source, destination);
  await chmod(destination, 0o755);
} else {
  fail("usage: release.mjs check [version] | stage <platform> <binary>");
}

async function checkVersions(expected) {
  const cargo = await readFile(join(root, "Cargo.toml"), "utf8");
  const cargoVersion = cargo.match(/^version = "([^"]+)"$/m)?.[1];
  if (!cargoVersion) {
    fail("could not read the Cargo package version");
  }
  if (expected && cargoVersion !== expected) {
    fail(`tag version ${expected} does not match Cargo version ${cargoVersion}`);
  }

  const packagePaths = [
    join(root, "npm", "astcount", "package.json"),
    ...platforms.map((platform) =>
      join(root, "npm", "platforms", platform, "package.json")
    )
  ];
  for (const path of packagePaths) {
    const manifest = JSON.parse(await readFile(path, "utf8"));
    if (manifest.version !== cargoVersion) {
      fail(`${path} has version ${manifest.version}; expected ${cargoVersion}`);
    }
  }

  const rootManifest = JSON.parse(await readFile(packagePaths[0], "utf8"));
  for (const [name, version] of Object.entries(rootManifest.optionalDependencies)) {
    if (version !== cargoVersion) {
      fail(`${name} dependency has version ${version}; expected ${cargoVersion}`);
    }
  }
}

function fail(message) {
  console.error(`astcount release: ${message}`);
  process.exit(1);
}
