/** biome-ignore-all lint/complexity/useLiteralKeys: 和 ts 配置 noPropertyAccessFromIndexSignature 冲突 */
/**
 * Build the plugin, install it as the only InfLink copy BetterNCM can load, and
 * restart NetEase Cloud Music with the debug port so the result can be checked.
 *
 *   pnpm dev-install              build, install, restart, verify
 *   pnpm dev-install --no-build   reuse packages/frontend/dist
 *   pnpm dev-install --no-start   leave the client closed afterwards
 *   pnpm dev-install --check      only confirm the running client loaded it
 *
 * Environment: BETTERNCM_PLUGIN_PATH, NCM_EXE, NCM_DEBUG_PORT, LIBCLANG_PATH.
 */

import { execFileSync, spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const projectRoot = path.resolve(
	path.dirname(fileURLToPath(import.meta.url)),
	"..",
);
const frontendDir = path.join(projectRoot, "packages/frontend");
const distDir = path.join(frontendDir, "dist");

const pluginDir =
	process.env["BETTERNCM_PLUGIN_PATH"] || "C:/betterncm/plugins_dev/InfLink-rs";
const betterNcmRoot = path.dirname(path.dirname(pluginDir));
const ncmExe =
	process.env["NCM_EXE"] ||
	"C:/Program Files/NetEase/CloudMusic/cloudmusic.exe";
const debugPort = process.env["NCM_DEBUG_PORT"] || "9223";
const defaultLibclang = "C:/Program Files/LLVM/bin";

const REQUIRED_FILES = [
	"index.js",
	"manifest.json",
	"backend.dll",
	"backend.dll.x64.dll",
];

const args = new Set(process.argv.slice(2));
const delay = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

function fail(message: string): never {
	console.error(`\n[dev-install] ${message}`);
	process.exit(1);
}

function step(message: string) {
	console.log(`\n[dev-install] ${message}`);
}

function build() {
	// bindgen needs libclang, and a default LLVM install does not put it on PATH
	if (
		!process.env["LIBCLANG_PATH"] &&
		fs.existsSync(path.join(defaultLibclang, "libclang.dll"))
	) {
		process.env["LIBCLANG_PATH"] = defaultLibclang;
	}

	step("building both architectures");
	const vite = path.join(projectRoot, "node_modules/vite/bin/vite.js");
	execFileSync(process.execPath, [vite, "build"], {
		cwd: frontendDir,
		stdio: "inherit",
	});
}

function checkArtifacts() {
	const missing = REQUIRED_FILES.filter(
		(name) => !fs.existsSync(path.join(distDir, name)),
	);
	if (missing.length > 0) {
		fail(`build output is incomplete, missing: ${missing.join(", ")}`);
	}
}

/**
 * A packaged copy next to the development copy means two plugins publishing
 * to Discord and SMTC at once, which looks like the presence is broken.
 */
function checkNoPackagedCopy() {
	const packagedDir = path.join(betterNcmRoot, "plugins");
	if (!fs.existsSync(packagedDir)) return;

	const packaged = fs
		.readdirSync(packagedDir)
		.filter((name) => /^inflink/i.test(name));
	if (packaged.length > 0) {
		fail(
			`a packaged InfLink is installed: ${packaged.join(", ")}\n` +
				`move it out of ${packagedDir} first, then run this again`,
		);
	}
}

function isNcmRunning(): boolean {
	const output = execFileSync(
		"tasklist",
		["/FI", "IMAGENAME eq cloudmusic.exe", "/NH"],
		{ encoding: "utf-8" },
	);
	return output.toLowerCase().includes("cloudmusic.exe");
}

/** The native DLL stays locked until every cloudmusic process is gone. */
async function stopNcm() {
	if (!isNcmRunning()) return;

	step("closing NetEase Cloud Music");
	try {
		execFileSync("taskkill", ["/F", "/IM", "cloudmusic.exe"], {
			stdio: "ignore",
		});
	} catch {
		// taskkill fails when the last process exits on its own first
	}

	for (let i = 0; i < 40; i++) {
		if (!isNcmRunning()) return;
		await delay(500);
	}
	fail("NetEase Cloud Music is still running, close it by hand and retry");
}

function install() {
	step(`installing to ${pluginDir}`);
	fs.mkdirSync(pluginDir, { recursive: true });
	for (const entry of fs.readdirSync(distDir)) {
		fs.cpSync(path.join(distDir, entry), path.join(pluginDir, entry), {
			recursive: true,
			force: true,
		});
	}
}

function startNcm() {
	step(`starting NetEase Cloud Music with debug port ${debugPort}`);
	// Started from its own folder: the client writes debug.log into its working directory
	const child = spawn(
		ncmExe,
		[
			`--remote-debugging-port=${debugPort}`,
			"--remote-debugging-address=127.0.0.1",
		],
		{ cwd: path.dirname(ncmExe), detached: true, stdio: "ignore" },
	);
	child.unref();
}

interface DebugTarget {
	type: string;
	url: string;
	webSocketDebuggerUrl: string;
}

async function evaluateInPage(expression: string): Promise<unknown> {
	const base = `http://127.0.0.1:${debugPort}`;

	let targets: DebugTarget[] = [];
	for (let i = 0; i < 30; i++) {
		try {
			const response = await fetch(`${base}/json/list`, {
				signal: AbortSignal.timeout(2000),
			});
			targets = (await response.json()) as DebugTarget[];
			if (targets.some((t) => t.type === "page")) break;
		} catch {
			// the port is not listening yet
		}
		await delay(1000);
	}

	const page = targets.find(
		(t) => t.type === "page" && t.url.startsWith("orpheus://"),
	);
	if (!page) fail("the client's page never showed up on the debug port");

	const socket = new WebSocket(page.webSocketDebuggerUrl);
	await new Promise((resolve, reject) => {
		socket.addEventListener("open", resolve, { once: true });
		socket.addEventListener("error", reject, { once: true });
	});

	try {
		return await new Promise((resolve, reject) => {
			const timer = setTimeout(
				() => reject(new Error("evaluation timed out")),
				40000,
			);
			socket.addEventListener("message", (event) => {
				const message = JSON.parse(String(event.data));
				if (message.id !== 1) return;
				clearTimeout(timer);
				if (message.error || message.result?.exceptionDetails) {
					reject(new Error(JSON.stringify(message.error ?? message.result)));
				} else {
					resolve(message.result.result.value);
				}
			});
			socket.send(
				JSON.stringify({
					id: 1,
					method: "Runtime.evaluate",
					params: { expression, returnByValue: true, awaitPromise: true },
				}),
			);
		});
	} finally {
		socket.close();
	}
}

interface LoadedState {
	version: string | null;
	plugins: string[];
}

/** Files being copied does not prove the running client loaded them. */
async function verifyLoaded() {
	step("waiting for the plugin to load");
	const expression = `(async () => {
		for (let i = 0; i < 60 && !window.InfLinkApi; i++) {
			await new Promise((resolve) => setTimeout(resolve, 500));
		}
		return {
			version: window.InfLinkApi ? window.InfLinkApi.version : null,
			plugins: Object.keys(window.loadedPlugins || {}),
		};
	})()`;

	// BetterNCM reloads the page while it starts up, which aborts an evaluation in flight
	let result: LoadedState | null = null;
	let lastError: unknown;
	for (let attempt = 0; attempt < 6 && !result; attempt++) {
		try {
			result = (await evaluateInPage(expression)) as LoadedState;
		} catch (e) {
			lastError = e;
			await delay(2000);
		}
	}
	if (!result) fail(`could not query the client: ${String(lastError)}`);

	const copies = result.plugins.filter((name) => /^inflink/i.test(name));
	if (!result.version) {
		fail(
			`InfLink did not load. loaded plugins: ${result.plugins.join(", ") || "none"}`,
		);
	}
	if (copies.length !== 1) {
		fail(`expected one InfLink plugin, found: ${copies.join(", ")}`);
	}
	console.log(
		`[dev-install] InfLink ${result.version} is loaded (${copies[0]})`,
	);
}

async function main() {
	if (args.has("--check")) {
		await verifyLoaded();
		return;
	}

	if (!fs.existsSync(ncmExe)) {
		fail(`NetEase Cloud Music not found at ${ncmExe}, set NCM_EXE`);
	}
	checkNoPackagedCopy();

	if (!args.has("--no-build")) build();
	checkArtifacts();

	await stopNcm();
	install();

	if (args.has("--no-start")) {
		console.log("\n[dev-install] installed, client left closed");
		return;
	}
	startNcm();
	await verifyLoaded();
}

await main();
