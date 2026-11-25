import { describe, expect, it } from 'vitest';
import type { DigFurtherPick, SetPick } from '$lib/features/discover/queries/week.svelte';
import { createWeekDeck } from './weekDeck.svelte';

function pick(n: number, state: SetPick['state'] = 'open'): SetPick {
	return {
		pick_no: n,
		album: { title: `Album ${n}`, musicbrainz_id: `rg-${n}`, in_library: false, requested: false },
		reason: { kind: 'your_artists', line: 'Close to your artists.', artists: [] },
		state,
		heard: false,
		taste_no: 0
	};
}

function extra(n: number): DigFurtherPick {
	return {
		album: { title: `Extra ${n}`, musicbrainz_id: `ex-${n}`, in_library: false, requested: false },
		reason: { kind: 'your_artists', line: 'Close to your artists.', artists: [] }
	};
}

const keys = (deck: ReturnType<typeof createWeekDeck>) => deck.current?.key ?? null;

describe('weekDeck', () => {
	it('starts on the first open pick and moves next, previous and by jump', () => {
		const deck = createWeekDeck();
		deck.sync([pick(1, 'later'), pick(2), pick(3)], []);
		expect(keys(deck)).toBe('rg-2');

		deck.next();
		expect(keys(deck)).toBe('rg-3');
		deck.next();
		expect(keys(deck)).toBe('rg-3');
		deck.previous();
		deck.previous();
		expect(keys(deck)).toBe('rg-1');
		deck.previous();
		expect(keys(deck)).toBe('rg-1');

		deck.jumpTo('rg-3');
		expect(keys(deck)).toBe('rg-3');
		deck.jumpTo('rg-9');
		expect(keys(deck)).toBe('rg-3');
	});

	it('moves past an answered pick to the next open one, wrapping round', () => {
		const deck = createWeekDeck();
		deck.sync([pick(1), pick(2, 'known'), pick(3), pick(4)], []);
		deck.jumpTo('rg-3');

		deck.answer('rg-3', 'asked');
		expect(keys(deck)).toBe('rg-4');
		deck.answer('rg-4', 'no');
		expect(keys(deck)).toBe('rg-1');
		expect(deck.stateOf(deck.entries[3])).toBe('no');
		expect(deck.answered).toBe(3);
	});

	it('keeps its place when the set is read again, and lets the read take over', () => {
		const deck = createWeekDeck();
		deck.sync([pick(1), pick(2), pick(3)], []);
		deck.answer('rg-1', 'asked');
		expect(keys(deck)).toBe('rg-2');

		// the next read still says open: the answer holds
		deck.sync([pick(1), pick(2), pick(3)], []);
		expect(keys(deck)).toBe('rg-2');
		expect(deck.stateOf(deck.entries[0])).toBe('asked');

		// it now says what became of the request, which shows instead
		deck.sync([pick(1, 'awaiting_approval'), pick(2), pick(3)], []);
		expect(keys(deck)).toBe('rg-2');
		expect(deck.stateOf(deck.entries[0])).toBe('awaiting_approval');
	});

	it('deals extras once every pick is answered, then is done', () => {
		const deck = createWeekDeck();
		deck.sync([pick(1), pick(2, 'no')], [extra(1), extra(2)]);
		deck.answer('rg-1', 'later');
		expect(deck.current).toMatchObject({ kind: 'extra', key: 'ex-1', n: 1 });

		deck.answer('ex-1', 'known');
		expect(deck.current).toMatchObject({ kind: 'extra', key: 'ex-2', n: 2 });
		deck.answer('ex-2', 'no');
		expect(deck.current).toBeNull();
		expect(deck.done).toBe(true);
	});

	it('puts a pick back on the counter when its answer is taken back', () => {
		const deck = createWeekDeck();
		deck.sync([pick(1), pick(2)], []);
		deck.answer('rg-1', 'no');
		expect(keys(deck)).toBe('rg-2');

		deck.back('rg-1');
		expect(keys(deck)).toBe('rg-1');
		expect(deck.stateOf(deck.entries[0])).toBe('open');
	});
});
