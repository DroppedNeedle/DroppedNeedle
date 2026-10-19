// Colours for a cover placeholder. The same seed (a MusicBrainz id, or a title
// when there is no id) always gets the same pair of tokens and angle, so an
// item looks the same on every screen. Used by CoverPlaceholder.svelte.

// Saturated theme tokens only. Neutral is left out: it is pale on light and
// dark on dark, so no icon colour reads on it in both themes.
const PALETTE = ['primary', 'secondary', 'accent', 'info', 'success', 'warning', 'error'] as const;
const ANGLES = [45, 135, 225, 315] as const;

// base-100 is the page colour inverted: it reads on every palette token in both
// themes.
const SATURATED_INK = 'text-base-100';
const NEUTRAL_INK = 'text-base-content';

export interface CoverPlaceholderLook {
	/** CSS background-image value built from theme tokens. */
	background: string;
	/** Tailwind text colour class for the icon on top. */
	ink: string;
}

const NEUTRAL: CoverPlaceholderLook = {
	background: 'linear-gradient(135deg, var(--color-base-300), var(--color-neutral))',
	ink: NEUTRAL_INK
};

// FNV-1a, then the murmur3 finaliser so every bit of the result depends on
// every character.
function hash(text: string): number {
	let h = 0x811c9dc5;
	for (let i = 0; i < text.length; i++) h = Math.imul(h ^ text.charCodeAt(i), 0x01000193);
	h ^= h >>> 16;
	h = Math.imul(h, 0x85ebca6b);
	h ^= h >>> 13;
	h = Math.imul(h, 0xc2b2ae35);
	h ^= h >>> 16;
	return h >>> 0;
}

/** Colours for a seed; an empty seed gets the neutral pair. */
export function coverPlaceholder(seed: string): CoverPlaceholderLook {
	const text = seed.trim().toLowerCase();
	if (!text) return NEUTRAL;
	const h = hash(text);
	const count = PALETTE.length;
	const from = h % count;
	const to = (from + 1 + (Math.floor(h / count) % (count - 1))) % count;
	const angle = ANGLES[Math.floor(h / (count * (count - 1))) % ANGLES.length];
	return {
		background: `linear-gradient(${angle}deg, var(--color-${PALETTE[from]}), var(--color-${PALETTE[to]}))`,
		ink: SATURATED_INK
	};
}
