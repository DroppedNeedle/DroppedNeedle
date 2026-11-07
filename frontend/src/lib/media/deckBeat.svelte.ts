import { prefersReducedMotion } from 'svelte/motion';

/**
 * The heartbeat that turns a deck of pictures: a count that goes up every
 * 4.2 seconds and holds still while the viewer asks for less motion. Call it
 * while a component starts; decks that share one stagger by an offset.
 */
export function createDeckBeat() {
	let cycle = $state(0);

	$effect(() => {
		if (prefersReducedMotion.current) return;
		const id = setInterval(() => (cycle += 1), 4200);
		return () => clearInterval(id);
	});

	return {
		get cycle() {
			return cycle;
		}
	};
}

/**
 * The `size` items a deck shows at `cycle`, back to front. The deck holds
 * still unless the pool outnumbers it, else pictures shuffle with nothing new
 * to show.
 */
export function deckSlots<T>(items: T[], cycle: number, size: number) {
	const n = items.length;
	const win = Math.min(size, n);
	const start = n <= size ? 0 : ((cycle % n) + n) % n;
	return Array.from({ length: win }, (_, position) => ({
		item: items[(start + position) % n]!,
		position
	}));
}
