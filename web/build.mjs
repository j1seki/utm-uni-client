// Build the frontend into web/dist.  The Rust binary embeds these files with
// include_str!, so this must run before `cargo build` when the UI changes.
import { build, context } from "esbuild";
import { copyFile, mkdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = dirname(fileURLToPath(import.meta.url));
const outdir = join(root, "dist");
const watch = process.argv.includes("--watch");

const common = {
  bundle: true,
  minify: !watch,
  sourcemap: watch ? "inline" : false,
  target: ["es2020", "safari15", "firefox102", "chrome100"],
  logLevel: "info",
};

async function main() {
  await mkdir(outdir, { recursive: true });
  await copyFile(join(root, "index.html"), join(outdir, "index.html"));

  if (watch) {
    const ctx = await context({
      ...common,
      entryPoints: [join(root, "src/app.ts")],
      outfile: join(outdir, "app.js"),
    });
    const cssCtx = await context({
      ...common,
      entryPoints: [join(root, "src/style.css")],
      outfile: join(outdir, "app.css"),
    });
    await Promise.all([ctx.watch(), cssCtx.watch()]);
    console.log("watching web/src…");
    return;
  }

  await build({
    ...common,
    entryPoints: [join(root, "src/app.ts")],
    outfile: join(outdir, "app.js"),
  });
  await build({
    ...common,
    entryPoints: [join(root, "src/style.css")],
    outfile: join(outdir, "app.css"),
  });
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
