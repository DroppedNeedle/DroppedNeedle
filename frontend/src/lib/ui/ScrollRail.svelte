<!--
	The way through a long list. On wide screens, a letter rail beside the
	list (only while it has a letter index): `#`, A to Z and other alphabets,
	in the list's order. Letters with no rows are greyed, the letter of the
	rows in view is lit, and a letter shows its position on hover and jumps
	there on a press. On phones, a thumb on the right edge shows while the
	page scrolls; dragging it jumps through the list with a bubble giving the
	letter and the position.
-->
<script module lang="ts">
	import type { components } from '$lib/api/v3/openapi';

	export type LetterBucket = components['schemas']['LetterBucket'];
	type LetterKey = components['schemas']['LetterKey'];

	const KEYS: LetterKey[] = [
		'#',
		...('ABCDEFGHIJKLMNOPQRSTUVWXYZ'.split('') as LetterKey[]),
		'other'
	];

	const shown = (key: LetterKey) => (key === 'other' ? '…' : key);
	const named = (key: LetterKey) =>
		key === '#' ? 'Numbers and symbols' : key === 'other' ? 'Other alphabets' : key;
</script>

<script lang="ts">
	interface Props {
		/** The list's letter index; none on an order that is not by name. */
		letters?: LetterBucket[] | null;
		total: number;
		/** The position of the first row in view. */
		top: number;
		/** Scroll the list to a position. */
		onjump: (position: number) => void;
	}

	let { letters, total, top, onjump }: Props = $props();

	const byKey = $derived(new Map((letters ?? []).map((bucket) => [bucket.key, bucket])));
	// A descending list gives its letters Z first; the rail follows the list.
	const descending = $derived.by(() => {
		const [first, second] = letters ?? [];
		return !!first && !!second && KEYS.indexOf(first.key) > KEYS.indexOf(second.key);
	});
	const keys = $derived(descending ? [...KEYS].reverse() : KEYS);

	function letterAt(position: number): LetterBucket | null {
		let found: LetterBucket | null = null;
		for (const bucket of letters ?? []) {
			if (bucket.position > position) break;
			found = bucket;
		}
		return found;
	}

	const current = $derived(letterAt(top)?.key ?? null);
	const place = (position: number) =>
		`${(position + 1).toLocaleString()} of ${total.toLocaleString()}`;

	let pointed = $state<LetterKey | null>(null);
	const pointedBucket = $derived(pointed ? (byKey.get(pointed) ?? null) : null);

	// The phone thumb: shown while the page scrolls and while it is dragged.
	let scrolling = $state(false);
	let dragAt = $state<number | null>(null);
	let track = $state<HTMLDivElement>();
	let hideTimer: ReturnType<typeof setTimeout> | undefined;
	let frame = 0;
	let pending = 0;
	const THUMB = 48;

	function scrolled(): void {
		scrolling = true;
		clearTimeout(hideTimer);
		hideTimer = setTimeout(() => (scrolling = false), 1200);
	}

	$effect(() => () => {
		clearTimeout(hideTimer);
		cancelAnimationFrame(frame);
	});

	const thumbAt = $derived(dragAt ?? top);
	const fraction = $derived(total > 1 ? Math.min(1, thumbAt / (total - 1)) : 0);
	const dragLetter = $derived(dragAt !== null ? letterAt(dragAt) : null);

	function positionFor(clientY: number): number {
		const rect = track?.getBoundingClientRect();
		if (!rect) return 0;
		const span = Math.max(1, rect.height - THUMB);
		const part = Math.min(1, Math.max(0, (clientY - rect.top - THUMB / 2) / span));
		return Math.round(part * Math.max(0, total - 1));
	}

	function jumpSoon(position: number): void {
		pending = position;
		if (frame) return;
		frame = requestAnimationFrame(() => {
			frame = 0;
			onjump(pending);
		});
	}

	function grab(event: PointerEvent): void {
		if (!track || total < 2) return;
		event.preventDefault();
		(event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
		dragAt = positionFor(event.clientY);
		jumpSoon(dragAt);
	}

	function drag(event: PointerEvent): void {
		if (dragAt === null) return;
		dragAt = positionFor(event.clientY);
		jumpSoon(dragAt);
	}

	function drop(): void {
		dragAt = null;
		scrolled();
	}

	// The thumb is a touch target only, hidden from assistive tech, so its
	// pointer handlers are listened to directly.
	let thumb = $state<HTMLDivElement>();
	$effect(() => {
		const node = thumb;
		if (!node) return;
		const handlers = [
			['pointerdown', grab],
			['pointermove', drag],
			['pointerup', drop],
			['pointercancel', drop]
		] as const;
		for (const [type, handler] of handlers) node.addEventListener(type, handler);
		return () => {
			for (const [type, handler] of handlers) node.removeEventListener(type, handler);
		};
	});
</script>

<svelte:window onscroll={scrolled} />

{#if letters && letters.length > 0}
	<nav class="dn-rail" aria-label="Jump to a letter">
		{#each keys as key (key)}
			{@const bucket = byKey.get(key)}
			<button
				type="button"
				class="dn-rail__letter"
				data-current={current === key || undefined}
				disabled={!bucket}
				aria-label={key === '#' || key === 'other' ? named(key) : `Jump to ${key}`}
				aria-current={current === key ? 'location' : undefined}
				onpointerenter={() => (pointed = key)}
				onpointerleave={() => (pointed = null)}
				onfocus={() => (pointed = key)}
				onblur={() => (pointed = null)}
				onclick={() => bucket && onjump(bucket.position)}
			>
				{shown(key)}
				{#if pointed === key && pointedBucket}
					<span class="dn-rail__bubble" aria-hidden="true">
						{shown(key)} · {place(pointedBucket.position)}
					</span>
				{/if}
			</button>
		{/each}
	</nav>
{/if}

{#if total > 1}
	<div
		bind:this={track}
		class="dn-thumb-track"
		data-shown={scrolling || dragAt !== null || undefined}
		aria-hidden="true"
	>
		<div class="dn-thumb" style:top="calc({fraction} * (100% - {THUMB}px))" bind:this={thumb}>
			<span class="dn-thumb__bar"></span>
			{#if dragAt !== null}
				<span class="dn-thumb__bubble">
					{#if dragLetter}<strong>{shown(dragLetter.key)}</strong>{/if}
					{place(dragAt)}
				</span>
			{/if}
		</div>
	</div>
{/if}

<style>
	.dn-rail {
		display: none;
	}
	@media (min-width: 768px) {
		.dn-rail {
			position: sticky;
			top: calc(4rem + var(--ms-safe-top) + 0.5rem);
			display: flex;
			flex-direction: column;
			align-self: flex-start;
			justify-content: space-between;
			max-height: calc(100dvh - 4rem - var(--ms-safe-top) - var(--ms-player-height) - 1rem);
			width: 1.75rem;
			flex-shrink: 0;
		}
	}

	.dn-rail__letter {
		position: relative;
		display: flex;
		min-height: 1rem;
		flex: 1 1 0;
		max-height: 1.5rem;
		align-items: center;
		justify-content: center;
		border-radius: 0.25rem;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		line-height: 1;
		color: var(--color-base-content);
		cursor: pointer;
	}
	.dn-rail__letter:hover:not(:disabled),
	.dn-rail__letter:focus-visible {
		background: var(--color-neutral);
		outline: none;
	}
	.dn-rail__letter:focus-visible {
		box-shadow: 0 0 0 2px var(--color-focus);
	}
	.dn-rail__letter:disabled {
		color: color-mix(in oklab, var(--color-muted) 45%, transparent);
		cursor: default;
	}
	.dn-rail__letter[data-current] {
		background: var(--color-action);
		color: var(--color-action-content);
	}

	.dn-rail__bubble,
	.dn-thumb__bubble {
		position: absolute;
		top: 50%;
		right: calc(100% + 0.5rem);
		translate: 0 -50%;
		padding: 0.25rem 0.5rem;
		border: 1px solid var(--color-edge);
		border-radius: 0.375rem;
		background: var(--color-base-300);
		color: var(--color-base-content);
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 600;
		white-space: nowrap;
		pointer-events: none;
		box-shadow: 0 8px 24px -12px rgb(0 0 0 / 0.6);
	}

	.dn-thumb-track {
		position: fixed;
		z-index: 30;
		top: calc(4rem + var(--ms-safe-top) + 0.5rem);
		right: 0;
		bottom: calc(var(--ms-bottom-nav-offset) + var(--ms-player-height) + 0.5rem);
		width: 2.75rem;
		opacity: 0;
		pointer-events: none;
		transition: opacity 200ms;
	}
	.dn-thumb-track[data-shown] {
		opacity: 1;
	}
	.dn-thumb-track[data-shown] .dn-thumb {
		pointer-events: auto;
	}
	@media (min-width: 768px) {
		.dn-thumb-track {
			display: none;
		}
	}

	.dn-thumb {
		position: absolute;
		right: 0;
		display: flex;
		width: 2.75rem;
		height: 3rem;
		justify-content: flex-end;
		padding-right: 0.25rem;
		touch-action: none;
		cursor: grab;
	}
	.dn-thumb__bar {
		width: 0.3125rem;
		height: 100%;
		border-radius: 999px;
		background: var(--color-action);
	}
	.dn-thumb__bubble strong {
		margin-right: 0.375rem;
		font-size: 1rem;
	}
</style>
