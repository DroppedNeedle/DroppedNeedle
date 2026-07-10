/*
 * playerUi — view-layer state for the rebuilt player system.
 *
 * Holds no playback logic: the Stage (full-screen player) open/closed state and
 * which side tab it shows. Playback itself stays entirely in playerStore.
 */

import { tick } from 'svelte';

export type StageTab = 'queue' | 'lyrics' | 'eq';

/* Stage open/close runs inside a view transition when the browser supports it,
   so the dock artwork morphs into the Stage artwork (both carry the
   `dn-now-art` view-transition-name). Falls back to the plain state flip. */
function withViewTransition(apply: () => void): void {
	if (typeof document === 'undefined') {
		apply();
		return;
	}
	const reduced =
		document.documentElement.dataset.dnMotion === 'reduced' ||
		(typeof matchMedia !== 'undefined' && matchMedia('(prefers-reduced-motion: reduce)').matches);
	const doc = document as Document & {
		startViewTransition?: (cb: () => Promise<void> | void) => unknown;
	};
	if (reduced || typeof doc.startViewTransition !== 'function') {
		apply();
		return;
	}
	doc.startViewTransition(async () => {
		apply();
		await tick();
	});
}

const SIDE_PANEL_KEY = 'dn-side-panel-open';

function readSidePanelPref(): boolean {
	if (typeof localStorage === 'undefined') return true;
	try {
		return localStorage.getItem(SIDE_PANEL_KEY) !== 'false';
	} catch {
		return true;
	}
}

class PlayerUiStore {
	stageOpen = $state(false);
	stageTab = $state<StageTab>('queue');
	/** ultrawide Now Playing side panel; defaults on, remembered per browser */
	sidePanelOpen = $state(readSidePanelPref());

	toggleSidePanel(): void {
		this.sidePanelOpen = !this.sidePanelOpen;
		try {
			localStorage.setItem(SIDE_PANEL_KEY, String(this.sidePanelOpen));
		} catch {
			/* private mode — preference just won't persist */
		}
	}

	openStage(tab?: StageTab): void {
		if (tab) this.stageTab = tab;
		withViewTransition(() => {
			this.stageOpen = true;
		});
	}

	closeStage(): void {
		withViewTransition(() => {
			this.stageOpen = false;
		});
	}

	toggleStage(tab?: StageTab): void {
		if (this.stageOpen && (!tab || this.stageTab === tab)) {
			this.stageOpen = false;
			return;
		}
		this.openStage(tab);
	}

	setTab(tab: StageTab): void {
		this.stageTab = tab;
	}
}

export const playerUi = new PlayerUiStore();
