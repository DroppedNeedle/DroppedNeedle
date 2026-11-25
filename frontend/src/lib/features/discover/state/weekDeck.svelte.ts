// Where the person is on the counter: the week's picks in their order,
// then the extras dealt from "dig further" once every pick is answered.
// The place is kept by album, so a set read again (with new states) keeps
// it. An answer shows at once; the week's next read carries the real state,
// and the deck lets it take over once it says something.
import { SvelteMap } from 'svelte/reactivity';
import type { DigFurtherPick, SetPick } from '$lib/features/discover/queries/week.svelte';
import type { StateLook } from '$lib/ui/StatusChip.svelte';

export type PickState = SetPick['state'];

export type DeckEntry =
	| { kind: 'pick'; key: string; pick: SetPick }
	| { kind: 'extra'; key: string; extra: DigFurtherPick; n: number };

/** How a state shows on a row, a sticker and the week laid out; none while open. */
export function lookOf(state: PickState): StateLook | undefined {
	switch (state) {
		case 'asked':
		case 'arrived':
			return 'kept';
		case 'later':
			return 'later';
		case 'known':
			return 'known';
		case 'no':
		case 'not_now':
		case 'not_artist':
			return 'passed';
		case 'awaiting_approval':
			return 'waiting';
		case 'refused':
			return 'refused';
		case 'failed':
			return 'missed';
		case 'open':
			return undefined;
	}
}

const keyOf = (pick: SetPick) => pick.album.musicbrainz_id ?? `pick-${pick.pick_no}`;
const extraKey = (extra: DigFurtherPick) => extra.album.musicbrainz_id ?? extra.album.title;

export function createWeekDeck() {
	let picks = $state.raw<SetPick[]>([]);
	let offered = $state.raw<DigFurtherPick[]>([]);
	let dealt = $state.raw<DigFurtherPick[]>([]);
	let currentKey = $state<string | null>(null);
	let started = false;
	// What this page answered, shown until the week's own read says something
	const answered = new SvelteMap<string, PickState>();

	const entries = $derived.by((): DeckEntry[] => [
		...picks.map((pick): DeckEntry => ({ kind: 'pick', key: keyOf(pick), pick })),
		...dealt.map((extra, i): DeckEntry => ({
			kind: 'extra',
			key: extraKey(extra),
			extra,
			n: i + 1
		}))
	]);

	function stateOf(entry: DeckEntry): PickState {
		return answered.get(entry.key) ?? (entry.kind === 'pick' ? entry.pick.state : 'open');
	}

	const isOpen = (entry: DeckEntry) => stateOf(entry) === 'open';
	const picksAnswered = $derived(
		entries.filter((entry) => entry.kind === 'pick' && !isOpen(entry)).length
	);
	const allPicksAnswered = $derived(picksAnswered === picks.length);

	/** The next extra on offer that is not on the counter yet. */
	function undealt(): DigFurtherPick | undefined {
		const taken = [...dealt.map(extraKey), ...picks.map(keyOf)];
		return offered.find((candidate) => !taken.includes(extraKey(candidate)));
	}

	/** The next extra not yet dealt, put on the counter. */
	function dealExtra(): boolean {
		const extra = undealt();
		if (!extra) return false;
		dealt = [...dealt, extra];
		currentKey = extraKey(extra);
		return true;
	}

	/** The first open entry after `from`, wrapping round; null when none is open. */
	function nextOpen(from: number): DeckEntry | null {
		const list = entries;
		for (let step = 1; step <= list.length; step++) {
			const entry = list[(from + step) % list.length];
			if (entry && isOpen(entry)) return entry;
		}
		return null;
	}

	/** Moves to the next open entry, dealing an extra once the picks are all answered. */
	function moveOn(from: number) {
		const entry = nextOpen(from);
		if (entry) {
			currentKey = entry.key;
			return;
		}
		if (allPicksAnswered && dealExtra()) return;
		currentKey = null;
	}

	const index = $derived(entries.findIndex((entry) => entry.key === currentKey));

	return {
		get entries() {
			return entries;
		},
		get current(): DeckEntry | null {
			return index >= 0 ? entries[index] : null;
		},
		get index() {
			return index;
		},
		/** How many of the week's picks are answered. */
		get answered() {
			return picksAnswered;
		},
		/** Whether "next" past the last entry deals an extra. */
		get extraLeft() {
			return allPicksAnswered && undealt() !== undefined;
		},
		/** Every pick, and every extra dealt, is answered and none is left to deal. */
		get done() {
			return picks.length > 0 && currentKey === null;
		},
		stateOf,

		/**
		 * Takes the week's picks and the extras on offer as read. The first
		 * call puts the first open pick on the counter; later calls keep the
		 * place, and an answer the read now carries gives way to it.
		 */
		sync(nextPicks: SetPick[], nextOffered: DigFurtherPick[]) {
			for (const pick of nextPicks) {
				const key = keyOf(pick);
				const mine = answered.get(key);
				if (mine === undefined) continue;
				if (mine === 'open' ? pick.state === 'open' : pick.state !== 'open') answered.delete(key);
			}
			picks = nextPicks;
			offered = nextOffered;
			dealt = dealt.map(
				(extra) => nextOffered.find((next) => extraKey(next) === extraKey(extra)) ?? extra
			);
			if (!started && nextPicks.length > 0) {
				started = true;
				moveOn(-1);
			} else if (started && currentKey === null && allPicksAnswered) {
				dealExtra();
			}
		},
		next() {
			if (index < 0) return;
			const following = entries[index + 1];
			if (following) currentKey = following.key;
			else if (allPicksAnswered) dealExtra();
		},
		previous() {
			if (index > 0) currentKey = entries[index - 1].key;
		},
		jumpTo(key: string) {
			if (entries.some((entry) => entry.key === key)) currentKey = key;
		},
		/** Records an answer on the entry `key` and moves past it when it is on the counter. */
		answer(key: string, state: PickState) {
			answered.set(key, state);
			if (key === currentKey) moveOn(index);
		},
		/** Takes an answer back and puts the entry on the counter again. */
		back(key: string) {
			answered.set(key, 'open');
			if (entries.some((entry) => entry.key === key)) currentKey = key;
		}
	};
}

export type WeekDeck = ReturnType<typeof createWeekDeck>;
