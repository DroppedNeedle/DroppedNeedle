<!--
	A panel below the fold that mounts its content only once it comes within
	about a screen of view, so the reads and images inside it wait until a
	person scrolls towards them. Until then an empty block of `minHeight`
	holds its place, and once shown the content stands in for it with no
	wrapper of its own. A section the page already passed shows too.
	`onshow` runs each time the content mounts; a new `key` (another album on
	the same page) puts the section back to waiting.
-->
<script lang="ts">
	import { untrack, type Snippet } from 'svelte';

	interface Props {
		children: Snippet;
		/** The placeholder's height, near the content's own. */
		minHeight?: string;
		/** What the content shows; a change waits for view again. */
		key?: unknown;
		onshow?: () => void;
	}

	let { children, minHeight = '8rem', key, onshow }: Props = $props();

	const waiting = Symbol('waiting');
	let shownFor = $state<unknown>(waiting);
	const shown = $derived(shownFor === key);
	let placeholder = $state<HTMLDivElement | null>(null);

	function show(): void {
		shownFor = key;
		untrack(() => onshow?.());
	}

	$effect(() => {
		const node = placeholder;
		if (!node || shown) return;
		// Within a screen of view, or already scrolled past, it shows at once.
		if (
			typeof IntersectionObserver === 'undefined' ||
			node.getBoundingClientRect().top < window.innerHeight * 2
		) {
			show();
			return;
		}
		const observer = new IntersectionObserver(
			([entry]) => {
				if (entry && (entry.isIntersecting || entry.boundingClientRect.top < 0)) {
					observer.disconnect();
					show();
				}
			},
			// The document's own view, so the margin holds inside a frame too.
			{ root: document, rootMargin: '100% 0px' }
		);
		observer.observe(node);
		return () => observer.disconnect();
	});
</script>

{#if shown}
	{@render children()}
{:else}
	<div bind:this={placeholder} style:min-height={minHeight} aria-hidden="true"></div>
{/if}
