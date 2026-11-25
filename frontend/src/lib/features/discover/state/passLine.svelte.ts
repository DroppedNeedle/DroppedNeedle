// The slip after an answer on the counter. "No" on a pick says "no" at
// once; the slip offers Undo, or one of "Know it already", "Not this
// artist" and "Not now" instead, and then says what that choice did, with
// an Undo that goes back one step. The page shows the slip over its
// receipt strip for eight seconds, held while it has focus or the pointer.
import {
	type DiscoverFeedbackEntry,
	type FeedbackItem,
	type FeedbackKind,
	tellDiscover,
	undoDiscoverFeedback
} from '$lib/features/discover/queries/feedback';
import { dayMonth } from '$lib/features/discover/state/weekWords';
import { applyEffects, recommendationsChanged } from '$lib/queries/effects';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';

/** How long the slip stays up. */
export const SLIP_MS = 8000;

export interface SlipAction {
	label: string;
	run: () => unknown;
}

export interface Slip {
	/** Changes with every slip shown, so the page restarts its time line. */
	id: number;
	title: string;
	message: string;
	/** Undo, when there is one. */
	action?: SlipAction;
	/** The other answers, after "Or was it". */
	choices?: SlipAction[];
}

function createSlip() {
	let current = $state<Slip | null>(null);
	let held = $state(false);
	let next = 0;
	let timer: ReturnType<typeof setTimeout> | null = null;
	let deadline = 0;
	let left = SLIP_MS;

	function clear() {
		if (timer) clearTimeout(timer);
		timer = null;
	}

	function run(ms: number) {
		clear();
		deadline = Date.now() + ms;
		timer = setTimeout(() => {
			timer = null;
			current = null;
			held = false;
		}, ms);
	}

	return {
		get current() {
			return current;
		},
		/** True while focus or the pointer keeps the slip up. */
		get held() {
			return held;
		},
		show(slip: Omit<Slip, 'id'>) {
			current = { ...slip, id: ++next };
			left = SLIP_MS;
			if (held) clear();
			else run(SLIP_MS);
		},
		dismiss() {
			clear();
			current = null;
			held = false;
		},
		hold() {
			if (held || !current) return;
			held = true;
			left = Math.max(0, deadline - Date.now());
			clear();
		},
		release() {
			if (!held) return;
			held = false;
			if (current) run(left);
		}
	};
}

/** The one slip on the page. */
export const pickSlip = createSlip();

function changed(): void {
	void applyEffects(recommendationsChanged(authStore.user?.id));
}

function failed(): void {
	toastStore.show({ message: "Couldn't save that.", type: 'error' });
}

function undoFailed(): void {
	toastStore.show({ message: "Couldn't undo that.", type: 'error' });
}

/** What a choice did, in the words the slip shows. */
function said(kind: FeedbackKind, item: FeedbackItem, expiresAt?: number | null) {
	switch (kind) {
		case 'know_it':
			return {
				title: 'Know it already.',
				message: 'Counts in your favour: more from around here, never this one again.'
			};
		case 'not_artist':
			return {
				title: 'Not this artist.',
				message: `No more from ${item.artistName} anywhere, until you undo it.`
			};
		case 'not_now':
			return {
				title: 'Not now.',
				message: expiresAt ? `Back after ${dayMonth(expiresAt * 1000)}.` : ''
			};
		case 'no':
			return { title: `Passed on ${item.releaseName}.`, message: '' };
	}
}

/** The first slip: "Passed on …", with Undo and the other answers. */
function showPassed(item: FeedbackItem, back: () => void): void {
	const choices: SlipAction[] = [
		{ label: 'Know it already', run: () => choose('know_it', item, back) }
	];
	if (item.artistMbid) {
		choices.push({ label: 'Not this artist', run: () => choose('not_artist', item, back) });
	}
	choices.push({ label: 'Not now', run: () => choose('not_now', item, back) });
	pickSlip.show({
		...said('no', item),
		action: { label: 'Undo', run: () => undoPass(item, back) },
		choices
	});
}

/** Undo on the first slip: the "no" goes and the pick comes back. */
async function undoPass(item: FeedbackItem, back: () => void): Promise<void> {
	pickSlip.dismiss();
	try {
		await undoDiscoverFeedback('album', item.releaseGroupMbid);
	} catch {
		undoFailed();
		return;
	}
	back();
	changed();
}

async function choose(kind: FeedbackKind, item: FeedbackItem, back: () => void) {
	let told: DiscoverFeedbackEntry;
	try {
		told = await tellDiscover(kind, item);
	} catch {
		failed();
		return;
	}
	changed();
	pickSlip.show({
		...said(kind, item, told.expires_at),
		action: { label: 'Undo', run: () => stepBack(kind, item, told, back) }
	});
}

/**
 * Undo after a choice goes back one step, to "Passed": the album's "no"
 * again, or, after "Not this artist", the artist's answer gone and the
 * album's "no" left standing.
 */
async function stepBack(
	kind: FeedbackKind,
	item: FeedbackItem,
	told: DiscoverFeedbackEntry,
	back: () => void
): Promise<void> {
	try {
		if (kind === 'not_artist') {
			await undoDiscoverFeedback(told.target, told.target_mbid);
		} else {
			await tellDiscover('no', item);
		}
	} catch {
		undoFailed();
		return;
	}
	changed();
	showPassed(item, back);
}

/**
 * Say "no" to an album the person was shown and show the slip after it.
 * The caller has already moved past the pick; `back` brings it back when
 * the answer is undone or cannot be saved.
 */
export async function passOn(item: FeedbackItem, back: () => void): Promise<void> {
	try {
		await tellDiscover('no', item);
	} catch {
		back();
		failed();
		return;
	}
	changed();
	showPassed(item, back);
}
