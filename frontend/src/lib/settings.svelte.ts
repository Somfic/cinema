import { browser } from "$app/environment";

export enum Fanciness {
	Potato = "potato",
	Ok = "ok",
	Fancy = "fancy",
	SuperFancy = "super-fancy",
}

const FancinessLevel = {
	[Fanciness.Potato]: 0,
	[Fanciness.Ok]: 1,
	[Fanciness.Fancy]: 2,
	[Fanciness.SuperFancy]: 3,

	isAtLeast(a: Fanciness, b: Fanciness): boolean {
		return FancinessLevel[a] >= FancinessLevel[b];
	},
};

const KEY = "cinema:fanciness";
const DEFAULT = Fanciness.Ok;
const CAST_KEY = "cinema:cast-capabilities";
const HDR_KEY = "cinema:keep-hdr";

/** What the Chromecast in use can decode beyond H.264 and AAC. Older
 *  Chromecasts do neither; a Chromecast with Google TV does all three. */
export interface CastCapabilitySettings {
	hevc: boolean;
	/** AC-3 / E-AC-3, passed through to the TV or receiver. */
	dolby: boolean;
	/** 4K output. */
	uhd: boolean;
}

const CAST_DEFAULT: CastCapabilitySettings = { hevc: false, dolby: false, uhd: false };

function isFanciness(v: unknown): v is Fanciness {
	return typeof v === "string" && v in FancinessLevel;
}

class Settings {
	fanciness = $state<Fanciness>(DEFAULT);
	cast = $state<CastCapabilitySettings>({ ...CAST_DEFAULT });
	/** Play HDR sources as HDR when the client can; off tone maps to SDR. */
	keepHdr = $state(true);
	animations = new Animations(this);

	constructor() {
		if (!browser) return;
		const stored = localStorage.getItem(KEY);
		if (isFanciness(stored)) this.fanciness = stored;
		this.keepHdr = localStorage.getItem(HDR_KEY) !== "false";
		try {
			const cast = JSON.parse(localStorage.getItem(CAST_KEY) ?? "null");
			if (cast && typeof cast === "object") this.cast = { ...CAST_DEFAULT, ...cast };
		} catch {
			// Keep the defaults.
		}
	}

	setFanciness(v: Fanciness): void {
		this.fanciness = v;
		if (browser) localStorage.setItem(KEY, v);
	}

	setKeepHdr(v: boolean): void {
		this.keepHdr = v;
		if (browser) localStorage.setItem(HDR_KEY, String(v));
	}

	setCast(v: Partial<CastCapabilitySettings>): void {
		this.cast = { ...this.cast, ...v };
		if (browser) localStorage.setItem(CAST_KEY, JSON.stringify(this.cast));
	}
}

class Animations {
	#settings: Settings;

	constructor(settings: Settings) {
		this.#settings = settings;
	}

	get glow(): boolean {
		return FancinessLevel.isAtLeast(this.#settings.fanciness, Fanciness.SuperFancy);
	}
}

export const settings = new Settings();
