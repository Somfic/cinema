// Validates the TV-remote WebSocket contract against a running server:
// presence broadcast, client→client command/state proxy, and the remote_* guard.
const URL = process.env.WS_URL ?? "ws://127.0.0.1:3000/api/ws";

function open(name) {
	const ws = new WebSocket(URL);
	ws.events = [];
	ws.addEventListener("message", (e) => {
		try {
			ws.events.push(JSON.parse(e.data));
		} catch {}
	});
	return new Promise((res, rej) => {
		ws.addEventListener("open", () => res(ws));
		ws.addEventListener("error", rej);
	});
}
const send = (ws, obj) => ws.send(JSON.stringify(obj));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const eventsOn = (ws, topic) => ws.events.filter((e) => e.topic === topic);

let failures = 0;
function check(label, cond) {
	console.log(`${cond ? "✅" : "❌"} ${label}`);
	if (!cond) failures++;
}

const tv = await open("tv");
const phone = await open("phone");

// Both watch presence.
send(tv, { type: "subscribe", topic: "remote_presence" });
send(phone, { type: "subscribe", topic: "remote_presence" });
await sleep(50);

send(tv, {
	type: "register",
	presence: { id: "tv1", label: "Living Room", kind: "desktop", width: 1920, mode: "browser", paired_to: null },
});
await sleep(50);
send(phone, {
	type: "register",
	presence: { id: "ph1", label: "Pixel", kind: "phone", width: 400, mode: "browser", paired_to: null },
});
await sleep(100);

const lastPresence = eventsOn(phone, "remote_presence").at(-1)?.payload ?? [];
check("presence lists both clients", lastPresence.length === 2 &&
	lastPresence.some((p) => p.id === "tv1") && lastPresence.some((p) => p.id === "ph1"));

const tvEntry = lastPresence.find((p) => p.id === "tv1");
const phEntry = lastPresence.find((p) => p.id === "ph1");
check("width is carried + phone is the smaller", tvEntry?.width === 1920 && phEntry?.width === 400 && phEntry.width < tvEntry.width);

// Phone pairs → TV must see paired_to===tv1 in presence.
send(phone, {
	type: "update",
	presence: { id: "ph1", label: "Pixel", kind: "phone", width: 400, mode: "remote", paired_to: "tv1" },
});
await sleep(100);
const tvView = eventsOn(tv, "remote_presence").at(-1)?.payload ?? [];
check("TV sees a remote paired to it", tvView.some((p) => p.id === "ph1" && p.paired_to === "tv1"));

// Command proxy: phone → remote_cmd_tv1 → TV (subscribed).
send(tv, { type: "subscribe", topic: "remote_cmd_tv1" });
await sleep(50);
send(phone, { type: "publish", topic: "remote_cmd_tv1", payload: { kind: "play_pause" } });
await sleep(100);
check("TV receives proxied command", eventsOn(tv, "remote_cmd_tv1").some((e) => e.payload?.kind === "play_pause"));

// State proxy: TV → remote_state_tv1 → phone (subscribed).
send(phone, { type: "subscribe", topic: "remote_state_tv1" });
await sleep(50);
send(tv, { type: "publish", topic: "remote_state_tv1", payload: { currentTime: 42, paused: false } });
await sleep(100);
check("phone receives proxied TV state", eventsOn(phone, "remote_state_tv1").some((e) => e.payload?.currentTime === 42));

// Guard: publishing to a non-remote_ topic must be rejected (not delivered).
send(tv, { type: "subscribe", topic: "streams_stats" });
await sleep(50);
send(phone, { type: "publish", topic: "streams_stats", payload: { spoof: true } });
await sleep(100);
check("non-remote_ publish is blocked", eventsOn(tv, "streams_stats").length === 0);

// Disconnect: closing the phone drops it from presence.
phone.close();
await sleep(150);
const afterClose = eventsOn(tv, "remote_presence").at(-1)?.payload ?? [];
check("disconnect removes client from roster", !afterClose.some((p) => p.id === "ph1"));

tv.close();
console.log(failures === 0 ? "\nALL PASSED" : `\n${failures} FAILED`);
process.exit(failures === 0 ? 0 : 1);
