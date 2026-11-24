// The share image's one look, copied from frontend/src/theme.css: the `dark`
// daisyUI theme, its `--dn-*` colours and the placeholder cover palette, and
// the font stacks from frontend/src/fonts.css. The image never reads the live
// theme or a person's custom CSS, so every saved image looks the same.
export const INK = {
	/** --color-base-100 */
	page: '#090e0b',
	/** --color-base-content */
	text: '#f3f5ef',
	/** --dn-muted */
	muted: '#bac9b8',
	/** --color-primary, --dn-title */
	acid: '#b8ef64',
	/** --color-primary-content */
	acidInk: '#121a12',
	/** --color-neutral, the raised surface: rules and the record's edge */
	rule: '#263128',
	/** --dn-line: the rank numbers */
	rank: '#4d604d',
	/** --dn-vinyl and --dn-vinyl-groove */
	vinyl: '#0b0e0c',
	groove: '#2b342b'
} as const;

/** --color-cover-*: the placeholder pairs coverPlaceholder() names. */
export const COVER: Record<string, string> = {
	'1': '#aed5f2',
	'2': '#f43098',
	'3': '#bbdb9b',
	'4': '#00bafe',
	'5': '#00d390',
	'6': '#fcb700',
	'7': '#ff627d',
	'neutral-from': '#1f271b',
	'neutral-to': '#09090b'
};

/** --color-cover-ink and --color-cover-ink-pale: the mark on a placeholder. */
export const COVER_INK = { saturated: '#121a12', pale: '#f3f5ef' } as const;

/** --font-display, --font-sans and --font-mono. */
export const FAMILY = {
	display: "Anton, Impact, 'Arial Narrow', sans-serif",
	sans: "Inter, system-ui, -apple-system, 'Segoe UI', Roboto, sans-serif",
	mono: "'IBM Plex Mono', ui-monospace, 'SF Mono', Menlo, monospace"
} as const;
