<!--
	A long list drawn by position. The box is as tall as every line, so the
	page's scroll bar shows where you are, and only the lines within half a
	screen of view are mounted. The page scrolls (window scroll); the pitch is
	fixed, read from the first line drawn. `layout="grid"` lays its items out
	by the caller's grid class and reads the number a line from it.
	`layout="rows"` is a table of rows (`aria-rowcount`, `aria-rowindex`); the
	grid a list (`aria-setsize`, `aria-posinset`). The item with focus stays
	mounted, and the arrow keys, Page Up and Down, Home and End move it by
	position.
-->
<script module lang="ts">
	export interface VirtualRange {
		/** The first item mounted and one past the last. */
		start: number;
		end: number;
		/** The first item at the top of the view. */
		top: number;
	}
</script>

<script lang="ts">
	import { tick, type Snippet } from 'svelte';

	interface Props {
		count: number;
		/** One line's height in px until a drawn line gives the real one. */
		pitch: number;
		layout?: 'rows' | 'grid';
		/** The list's accessible name. */
		label: string;
		/** Space a jump keeps clear above its line (the sticky top bar), in px. */
		scrollMargin?: number;
		/** Classes of the box the items sit in: a grid's template, borders. */
		class?: string;
		/**
		 * The item at a position, or a placeholder: an element marked
		 * `data-placeholder` with `tabindex="-1"`, so focus can wait on it.
		 */
		item: Snippet<[number]>;
		onrange?: (range: VirtualRange) => void;
	}

	let {
		count,
		pitch: guessPitch,
		layout = 'rows',
		label,
		scrollMargin = 80,
		class: className = '',
		item,
		onrange
	}: Props = $props();

	let box = $state<HTMLDivElement>();
	let inner = $state<HTMLDivElement>();
	let measuredPitch = $state(0);
	let columns = $state(1);
	let rowGap = $state(0);
	let startLine = $state(0);
	let endLine = $state(0);
	let topLine = $state(0);
	let focusIndex = $state<number | null>(null);

	const pitch = $derived(measuredPitch || guessPitch);
	const perLine = $derived(layout === 'grid' ? columns : 1);
	const itemHeight = $derived(
		layout === 'grid' && measuredPitch ? `${measuredPitch - rowGap}px` : undefined
	);
	const lines = $derived(Math.ceil(count / perLine));
	const start = $derived(Math.min(count, startLine * perLine));
	const end = $derived(Math.min(count, endLine * perLine));
	const indices = $derived(Array.from({ length: end - start }, (_, i) => start + i));
	// The item with focus stays in the same element while the window moves
	// away from it, laid where it belongs, so focus is never lost.
	const mounted = $derived(
		focusIndex !== null && focusIndex < count && (focusIndex < start || focusIndex >= end)
			? [...indices, focusIndex]
			: indices
	);

	// The lines in view and half a screen either side, from where the box sits now.
	function measure(): void {
		if (!box) return;
		const view = window.innerHeight;
		const into = -box.getBoundingClientRect().top;
		const clampLine = (line: number) => Math.min(lines, Math.max(0, line));
		startLine = clampLine(Math.floor((into - view / 2) / pitch));
		endLine = Math.max(startLine, clampLine(Math.ceil((into + 1.5 * view) / pitch)));
		topLine = clampLine(Math.floor((into + scrollMargin) / pitch));
	}

	// The line's height and the items a line, from what the browser drew. A
	// row's pitch is the distance from one row to the next, so borders count;
	// a grid's is a loaded item's height and the gap, and every item in it
	// keeps that height, so a line still loading does not move the rest.
	function readLayout(): void {
		if (!inner) return;
		const style = getComputedStyle(inner);
		let measured = 0;
		if (layout === 'grid') {
			columns = Math.max(1, style.gridTemplateColumns.split(' ').filter(Boolean).length);
			rowGap = parseFloat(style.rowGap) || 0;
			const loaded = inner.querySelector<HTMLElement>(
				':scope > [data-index] > :not([data-placeholder])'
			);
			if (loaded) measured = loaded.getBoundingClientRect().height + rowGap;
		} else {
			const items = inner.querySelectorAll<HTMLElement>(':scope > [data-index]');
			const [first, next] = items;
			if (first) {
				measured = next
					? next.getBoundingClientRect().top - first.getBoundingClientRect().top
					: first.getBoundingClientRect().height;
			}
		}
		if (measured > 0) measuredPitch = measured;
	}

	let frame = 0;
	function schedule(): void {
		if (frame) return;
		frame = requestAnimationFrame(() => {
			frame = 0;
			measure();
		});
	}

	$effect(() => () => cancelAnimationFrame(frame));

	// A new count, pitch or width moves the window at once.
	$effect(() => {
		void lines;
		void pitch;
		measure();
	});

	$effect(() => {
		if (!box) return;
		const observer = new ResizeObserver(() => {
			readLayout();
			measure();
		});
		observer.observe(box);
		return () => observer.disconnect();
	});

	// The lines drawn give the pitch, once there are two of them.
	$effect(() => {
		if (indices.length > 0) readLayout();
	});

	// Keys move by position, listened to on the list as a whole.
	$effect(() => {
		const list = inner;
		if (!list) return;
		const listen = (event: KeyboardEvent) => void keydown(event);
		list.addEventListener('keydown', listen);
		return () => list.removeEventListener('keydown', listen);
	});

	$effect(() => {
		onrange?.({ start, end, top: Math.min(count ? count - 1 : 0, topLine * perLine) });
	});

	/** Scrolls the list so the line holding `index` sits under the top bar. */
	export function scrollToIndex(index: number): void {
		if (!box) return;
		const line = Math.floor(Math.max(0, Math.min(index, count - 1)) / perLine);
		const top = box.getBoundingClientRect().top + window.scrollY + line * pitch - scrollMargin;
		window.scrollTo({ top: Math.max(0, top), behavior: 'instant' });
		measure();
	}

	function itemAt(index: number): HTMLElement | null {
		return inner?.querySelector<HTMLElement>(`:scope > [data-index="${index}"]`) ?? null;
	}

	// An item's main target, or, while it loads, its placeholder.
	function focusTarget(element: HTMLElement): HTMLElement | null {
		return (
			element.querySelector<HTMLElement>(
				'a[href], button:not([disabled]), [tabindex]:not([tabindex="-1"])'
			) ?? element.querySelector<HTMLElement>('[data-placeholder]')
		);
	}

	// The item whose placeholder holds focus: once it loads, its own target
	// takes the focus.
	let loadingFocus: number | null = null;
	$effect(() => {
		const list = inner;
		if (!list) return;
		const observer = new MutationObserver(() => {
			if (loadingFocus === null) return;
			const item = itemAt(loadingFocus);
			const target = item && focusTarget(item);
			if (!target || target.hasAttribute('data-placeholder')) return;
			loadingFocus = null;
			target.focus({ preventScroll: true });
		});
		observer.observe(list, { childList: true, subtree: true });
		return () => observer.disconnect();
	});

	function indexOf(target: EventTarget | null): number | null {
		const element = (target as Element | null)?.closest?.('[data-index]');
		if (!element || !inner?.contains(element)) return null;
		return Number(element.getAttribute('data-index'));
	}

	function focusin(event: FocusEvent): void {
		focusIndex = indexOf(event.target);
		const loading = (event.target as Element | null)?.hasAttribute?.('data-placeholder');
		loadingFocus = loading ? focusIndex : null;
	}

	function focusout(event: FocusEvent): void {
		const to = event.relatedTarget as Node | null;
		if (inner?.contains(to)) return;
		// A placeholder removed as its row loads keeps the place; focus moved
		// elsewhere gives it up.
		if (to === null && loadingFocus !== null) return;
		loadingFocus = null;
		focusIndex = null;
	}

	async function keydown(event: KeyboardEvent): Promise<void> {
		const from = indexOf(event.target);
		if (from === null) return;
		const own = itemAt(from);
		// Only from an item's main target, so a menu inside it keeps its keys.
		if (!own || focusTarget(own) !== event.target) return;
		const screen = Math.max(1, Math.floor(window.innerHeight / pitch) - 1) * perLine;
		const moves: Record<string, number> = {
			ArrowDown: from + perLine,
			ArrowUp: from - perLine,
			PageDown: from + screen,
			PageUp: from - screen,
			Home: 0,
			End: count - 1,
			...(layout === 'grid' ? { ArrowRight: from + 1, ArrowLeft: from - 1 } : {})
		};
		if (!(event.key in moves)) return;
		event.preventDefault();
		const to = Math.max(0, Math.min(count - 1, moves[event.key]));
		if (to === from) return;
		focusIndex = to;
		const line = Math.floor(to / perLine);
		const boxTop = box?.getBoundingClientRect().top ?? 0;
		const lineTop = boxTop + line * pitch;
		if (lineTop < scrollMargin || lineTop + pitch > window.innerHeight) scrollToIndex(to);
		await tick();
		const next = itemAt(to);
		const target = next && focusTarget(next);
		target?.focus({ preventScroll: true });
	}
</script>

<svelte:window onscroll={schedule} onresize={schedule} />

<div bind:this={box} class="relative" style:height="{lines * pitch}px">
	<div
		bind:this={inner}
		class={className}
		role={layout === 'rows' ? 'table' : 'list'}
		aria-label={label}
		aria-rowcount={layout === 'rows' ? count : undefined}
		style:transform="translateY({startLine * pitch}px)"
		onfocusin={focusin}
		onfocusout={focusout}
	>
		{#each mounted as index (index)}
			{@const kept = index < start || index >= end}
			<div
				data-index={index}
				role={layout === 'rows' ? 'row' : 'listitem'}
				aria-rowindex={layout === 'rows' ? index + 1 : undefined}
				aria-setsize={layout === 'grid' ? count : undefined}
				aria-posinset={layout === 'grid' ? index + 1 : undefined}
				class={kept ? 'absolute' : undefined}
				style:top={kept ? `${(Math.floor(index / perLine) - startLine) * pitch}px` : undefined}
				style:left={kept ? `${((index % perLine) * 100) / perLine}%` : undefined}
				style:width={kept ? `${100 / perLine}%` : undefined}
				style:height={itemHeight}
			>
				{@render item(index)}
			</div>
		{/each}
	</div>
</div>
