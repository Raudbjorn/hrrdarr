// Real endpoints on the owned library_ui_fixture; a local node receiver stands in for the remote
// webhook so delivery through the real outbox worker is observable.
import assert from "node:assert/strict";
import http from "node:http";
import { mkdir } from "node:fs/promises";
import { pathToFileURL } from "node:url";
const { chromium } = await import(
	pathToFileURL(
		process.env.PLAYWRIGHT_MODULE ||
			"/usr/lib/node_modules/playwright/index.mjs",
	).href
);
const origin = process.env.UI_URL,
	scratch = process.env.UI_SCRATCH,
	shots = process.env.UI_SCREENSHOT_DIR;
assert.ok(origin && scratch);
assert.equal(new URL(origin).hostname, "127.0.0.1");
const received = [];
const receiver = http.createServer((req, res) => {
	let body = "";
	req.on("data", (c) => (body += c));
	req.on("end", () => {
		received.push({ url: req.url, body: body ? JSON.parse(body) : null });
		res.end("ok");
	});
});
await new Promise((r) => receiver.listen(0, "127.0.0.1", r));
const hook = `http://127.0.0.1:${receiver.address().port}`;
const browser = await chromium.launch({
	headless: true,
	...(process.env.PLAYWRIGHT_EXECUTABLE
		? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE }
		: {}),
});
const page = await browser.newPage();
page.setDefaultTimeout(15000);
const errors = [];
page.on("pageerror", (e) => errors.push(e.message));
const panel = page.getByRole("region", { name: "Notifications", exact: true });
async function api(path) {
	const r = await page.request.get(`${origin}${path}`);
	assert.equal(r.status(), 200);
	return r.json();
}
async function until(check, label) {
	for (let i = 0; i < 60; i++) {
		if (await check()) return;
		await new Promise((r) => setTimeout(r, 500));
	}
	assert.fail(`timed out: ${label}`);
}
try {
	await page.goto(`${origin}/#/settings/connect`);
	await panel.getByText("No notifications are configured.").waitFor();
	// Add a webhook: domain-specific triggers, write-only password.
	const options = await panel
		.getByLabel("Add notification")
		.locator("option")
		.allTextContents();
	assert.ok(options.includes("Webhook"), `options: ${options}`);
	await panel.getByLabel("Add notification").selectOption({ label: "Webhook" });
	await panel.getByLabel("Name", { exact: true }).fill("Hook");
	await panel.getByLabel(/^URL/).fill(`${hook}/events`);
	await panel.getByLabel("Username", { exact: true }).fill("user");
	assert.equal(
		await panel.getByLabel("Password", { exact: true }).getAttribute("type"),
		"password",
	);
	await panel.getByLabel("Password", { exact: true }).fill("hunter2-secret");
	const tv = panel.getByRole("group", { name: "TV scope" }),
		movies = panel.getByRole("group", { name: "Movies scope" });
	assert.equal(await tv.getByLabel("On movie added").count(), 0);
	await tv.getByLabel("On series add", { exact: true }).check();
	await tv.getByLabel("On upgrade", { exact: true }).check();
	await panel.getByRole("button", { name: "Save", exact: true }).click();
	await panel.getByRole("alert").waitFor();
	assert.match(
		await panel.getByRole("alert").innerText(),
		/requires its base trigger/,
		"client mirror blocks upgrade without import",
	);
	await tv.getByLabel("On upgrade", { exact: true }).uncheck();
	// Test from the draft before saving.
	await panel.getByRole("button", { name: "Test", exact: true }).click();
	await panel.getByLabel("Test result").waitFor();
	assert.equal(
		await panel.getByLabel("Test result").innerText(),
		"TV: delivered",
	);
	await until(
		() => received.some((r) => r.body?.eventType === "Test"),
		"test delivery",
	);
	await panel.getByRole("button", { name: "Save", exact: true }).click();
	await panel.getByText('Notification "Hook" saved.').waitFor();
	const saved = (await api("/api/v1/notifications")).items;
	assert.equal(saved.length, 1);
	assert.deepEqual(saved[0].secrets_configured, ["password"]);
	assert.ok(!JSON.stringify(saved).includes("hunter2"));
	assert.equal(saved[0].movies, null);
	assert.equal(saved[0].tv.triggers.on_series_add, true);
	// Edit shows the secret as configured, never its value.
	await panel.getByRole("button", { name: "Edit Hook", exact: true }).click();
	assert.equal(
		await panel.getByLabel("Password", { exact: true }).inputValue(),
		"",
	);
	assert.match(
		await panel
			.getByLabel("Password", { exact: true })
			.getAttribute("placeholder"),
		/Configured/,
	);
	await page.screenshot({ path: `${shots}/notifications-edit.png` });
	await panel.getByRole("button", { name: "Cancel", exact: true }).click();
	// A real library add flows through the trigger, outbox and worker to the receiver.
	await mkdir(`${scratch}/notify-tv`, { recursive: true });
	const add = await page.request.post(`${origin}/api/v1/tv/series/lookup`, {
		data: {
			tvdb_id: 202,
			path: `${scratch}/notify-tv`,
			settings: { monitored: true },
		},
	});
	assert.equal(add.status(), 201, await add.text());
	await until(
		() => received.some((r) => r.body?.eventType === "SeriesAdd"),
		"series add delivery",
	);
	const event = received.find((r) => r.body?.eventType === "SeriesAdd").body;
	assert.equal(event.mediaType, "tv");
	assert.ok(event.series.title);
	assert.equal(event.movie, undefined);
	await page.reload();
	await panel.getByText(/add \(tv\): delivered/).waitFor();
	await page.screenshot({ path: `${shots}/notifications-list.png` });
	// Saved-notification test and deletion.
	await panel.getByRole("button", { name: "Test Hook", exact: true }).click();
	await panel.getByText("Hook: TV: delivered").waitFor();
	page.once("dialog", (d) => d.accept());
	await panel.getByRole("button", { name: "Delete Hook", exact: true }).click();
	await panel.getByText("No notifications are configured.").waitFor();
	assert.equal((await api("/api/v1/notifications")).items.length, 0);
	// A tag created in Settings -> Tags reaches the scope choices without a page reload, and the
	// draft in progress survives that catalog refresh.
	const nav = page.getByRole("navigation", { name: "Workspace", exact: true });
	await panel.getByLabel("Add notification").selectOption({ label: "Webhook" });
	await panel.getByLabel("Name", { exact: true }).fill("Draft in progress");
	const tvScope = panel.getByRole("group", { name: "TV scope", exact: true });
	const tvTags = tvScope.getByRole("group", { name: "TV tags", exact: true });
	await tvTags.waitFor();
	assert.equal(
		await tvTags.getByLabel("notify-tag", { exact: true }).count(),
		0,
		"The tag does not exist yet",
	);
	await nav.getByRole("button", { name: "Tags", exact: true }).click();
	const tagSettings = page.getByRole("region", {
		name: "Tags settings",
		exact: true,
	});
	await page.waitForFunction(() => {
		const f = document.querySelector(
			'section[aria-label="Tags settings"] fieldset',
		);
		return f && !f.disabled;
	});
	await tagSettings.getByLabel("Tag label", { exact: true }).fill("notify-tag");
	await tagSettings
		.getByRole("button", { name: "Create tag", exact: true })
		.click();
	await tagSettings.getByText("Tag saved.", { exact: true }).waitFor();
	await nav.getByRole("button", { name: "Connect", exact: true }).click();
	assert.equal(
		await panel.getByLabel("Name", { exact: true }).inputValue(),
		"Draft in progress",
		"The catalog refresh keeps the draft",
	);
	await tvTags.getByLabel("notify-tag", { exact: true }).waitFor();
	await tvTags.getByLabel("notify-tag", { exact: true }).check();
	assert.equal(
		await tvTags.getByLabel("notify-tag", { exact: true }).isChecked(),
		true,
		"The new tag is selectable",
	);
	await panel.getByRole("button", { name: "Cancel", exact: true }).click();
	await panel.getByText("No notifications are configured.").waitFor();
	assert.deepEqual(errors, []);
	console.log(
		"Notifications browser passed: add/validate/test/save, write-only secret, tv-only triggers, outbox delivery of a real series add, test saved, delete, tag catalog refresh with a retained draft.",
	);
} catch (error) {
	await page
		.screenshot({ path: `${shots}/notifications-failure.png` })
		.catch(() => {});
	throw error;
} finally {
	await browser.close();
	receiver.close();
}
