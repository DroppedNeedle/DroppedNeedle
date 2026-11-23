// "No" on a pick, and the line after it. Passing on an album says "no" at
// once; the line offers Undo, or one of "Know it already", "Not this
// artist" and "Not now" instead, and then says what that choice did, with
// an Undo that goes back one step. The deck and Top Picks share it, each
// with its own way of taking picks away and back.
import {
	type DiscoverFeedbackEntry,
	type FeedbackItem,
	type FeedbackKind,
	tellDiscover,
	undoDiscoverFeedback
} from '$lib/features/discover/queries/DiscoverQueueQueries';
import { applyEffects, recommendationsChanged } from '$lib/queries/effects';
import { authStore } from '$lib/stores/authStore.svelte';
import { type ToastAction, toastStore } from '$lib/stores/toast';

/** How long the line stays up. */
const LINE_MS = 8000;

/** Each kind of answer in words, as the list of them shows it. */
export const FEEDBACK_WORDS: Record<FeedbackKind, string> = {
	no: 'Passed',
	know_it: 'Know it already',
	not_now: 'Not now',
	not_artist: 'Not this artist'
};

/** A day and month, such as "22 Feb", from unix seconds. */
export function dayAndMonth(seconds: number): string {
	return new Date(seconds * 1000).toLocaleDateString(undefined, {
		day: 'numeric',
		month: 'short'
	});
}

function changed(): void {
	void applyEffects(recommendationsChanged(authStore.user?.id));
}

function failed(): void {
	toastStore.show({ message: "Couldn't save that.", type: 'error' });
}

/** What a choice did, in the words the line shows. */
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
				message: expiresAt ? `Back after ${dayAndMonth(expiresAt)}.` : ''
			};
		case 'no':
			return { title: `Passed on ${item.releaseName}.`, message: '' };
	}
}

/** How the surface that showed a pick takes it, and its artist, away and back. */
export interface PassActions {
	/** Put the passed pick back. */
	restore: () => void;
	/** Take every pick filed under this artist away; answers how to put them back. */
	dropArtist?: (artistMbid: string) => () => void;
}

function undoFailed(): void {
	toastStore.show({ message: "Couldn't undo that.", type: 'error' });
}

/** The first line: "Passed on …", with Undo and the other answers. */
function showPassed(item: FeedbackItem, actions: PassActions): void {
	const choices: ToastAction[] = [
		{ label: 'Know it already', run: () => choose('know_it', item, actions) }
	];
	if (item.artistMbid) {
		choices.push({ label: 'Not this artist', run: () => choose('not_artist', item, actions) });
	}
	choices.push({ label: 'Not now', run: () => choose('not_now', item, actions) });
	toastStore.show({
		type: 'info',
		duration: LINE_MS,
		...said('no', item),
		action: { label: 'Undo', run: () => undoPass(item, actions) },
		choices: { ask: 'Or was it', actions: choices }
	});
}

/** Undo on the first line: the "no" goes and the pick comes back. */
async function undoPass(item: FeedbackItem, actions: PassActions): Promise<void> {
	try {
		await undoDiscoverFeedback('album', item.releaseGroupMbid);
	} catch {
		undoFailed();
		return;
	}
	actions.restore();
	changed();
}

async function choose(kind: FeedbackKind, item: FeedbackItem, actions: PassActions) {
	let told: DiscoverFeedbackEntry;
	try {
		told = await tellDiscover(kind, item);
	} catch {
		failed();
		return;
	}
	changed();
	const putBack =
		kind === 'not_artist' && item.artistMbid ? actions.dropArtist?.(item.artistMbid) : undefined;
	toastStore.show({
		type: 'info',
		duration: LINE_MS,
		...said(kind, item, told.expires_at),
		action: { label: 'Undo', run: () => stepBack(kind, item, told, actions, putBack) }
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
	actions: PassActions,
	putBack: (() => void) | undefined
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
	putBack?.();
	changed();
	showPassed(item, actions);
}

/**
 * Say "no" to an album the person was shown and show the line after it.
 * The caller has already taken the pick away; `actions.restore` puts it
 * back when the answer is undone or cannot be saved.
 */
export async function passOn(item: FeedbackItem, actions: PassActions): Promise<void> {
	try {
		await tellDiscover('no', item);
	} catch {
		actions.restore();
		failed();
		return;
	}
	changed();
	showPassed(item, actions);
}
