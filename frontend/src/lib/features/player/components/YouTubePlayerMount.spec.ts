import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

/**
 * YouTube's Developer Policies forbid hiding or obscuring the embedded player, and an
 * audio-only experience built on it. `<YouTubePlayer />` was previously wrapped in
 * `<div class="hidden md:block">`, so below the md breakpoint the video was display:none
 * while the audio played on. That is the prohibited case, exactly.
 *
 * The component positions itself `fixed`, but a `display:none` ancestor still hides a fixed
 * child, so the constraint lives in Player.svelte and cannot be tested from inside the
 * component. Guard the source instead.
 */
const PLAYER_SOURCE = readFileSync(
	fileURLToPath(new URL('./Player.svelte', import.meta.url)),
	'utf8'
);

const HIDING_UTILITIES = new Set(['hidden', 'invisible']);

/**
 * A Tailwind class hides an element when its variant-stripped name is `hidden` or
 * `invisible`, so `md:hidden` counts and `overflow-hidden` does not. Matching the bare
 * word would flag `overflow-hidden`, because a hyphen is a regex word boundary.
 */
function hidingClassesIn(markup: string): string[] {
	return [...markup.matchAll(/class="([^"]*)"/g)]
		.flatMap(([, value]) => value.split(/\s+/))
		.filter((token) => HIDING_UTILITIES.has(token.split(':').pop() ?? ''));
}

describe('YouTubePlayer mount point in Player.svelte', () => {
	it('is rendered, not conditionally mounted away', () => {
		expect(PLAYER_SOURCE).toContain('<YouTubePlayer />');
	});

	it('is not wrapped in an element that could hide it', () => {
		const lines = PLAYER_SOURCE.split('\n');
		const at = lines.findIndex((line) => line.includes('<YouTubePlayer'));
		expect(at).toBeGreaterThan(-1);

		// only the immediately enclosing markup; a wrapper further out would be missed
		const enclosing = lines.slice(Math.max(0, at - 3), at).join('\n');

		expect(
			hidingClassesIn(enclosing),
			'YouTubePlayer must never sit inside a hiding wrapper'
		).toEqual([]);
	});
});
