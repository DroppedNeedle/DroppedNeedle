// Colours for a cover placeholder. The same seed (a MusicBrainz id, or a title
// when there is no id) always gets the same pair of tokens and angle, so an
// item looks the same on every screen. Used by CoverPlaceholder.svelte.

// The cover tokens in theme.css, the same in both themes, so the Subsonic
// placeholder the server draws (compat's PLACEHOLDER_PALETTE) matches.
const PALETTE = ['1', '2', '3', '4', '5', '6', '7'] as const;
const ANGLES = [45, 135, 225, 315] as const;

const SATURATED_INK = 'text-(--color-cover-ink)';
const NEUTRAL_INK = 'text-(--color-cover-ink-pale)';

export interface CoverPlaceholderLook {
	/** CSS background-image value built from theme tokens. */
	background: string;
	/** Tailwind text colour class for the icon on top. */
	ink: string;
}

const NEUTRAL: CoverPlaceholderLook = {
	background:
		'linear-gradient(135deg, var(--color-cover-neutral-from), var(--color-cover-neutral-to))',
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
		background: `linear-gradient(${angle}deg, var(--color-cover-${PALETTE[from]}), var(--color-cover-${PALETTE[to]}))`,
		ink: SATURATED_INK
	};
}
