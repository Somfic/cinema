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

function isFanciness(v: unknown): v is Fanciness {
	return typeof v === "string" && v in FancinessLevel;
}

class Settings {
	fanciness = $state<Fanciness>(DEFAULT);
	animations = new Animations(this);

	constructor() {
		if (!browser) return;
		const stored = localStorage.getItem(KEY);
		if (isFanciness(stored)) this.fanciness = stored;
	}

	setFanciness(v: Fanciness): void {
		this.fanciness = v;
		if (browser) localStorage.setItem(KEY, v);
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
