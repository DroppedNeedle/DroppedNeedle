<!-- A nested "advanced" disclosure inside a management profile editor section. -->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import { ChevronRight } from 'lucide-svelte';

	interface Props {
		title: string;
		subtitle: string;
		/** Layout only, e.g. a grid column span. */
		class?: string;
		children: Snippet;
	}

	let { title, subtitle, class: className = '', children }: Props = $props();
</script>

<details class="management-editor-advanced {className}">
	<summary>
		<span><strong>{title}</strong><small>{subtitle}</small></span>
		<ChevronRight class="h-4 w-4" />
	</summary>
	{@render children()}
</details>

<style>
	.management-editor-advanced > summary > span {
		display: grid;
		gap: 0.12rem;
	}

	.management-editor-advanced small {
		color: color-mix(in oklab, var(--color-base-content) 52%, transparent);
		font-size: 0.7rem;
		font-weight: 400;
	}

	.management-editor-advanced > summary:focus-visible {
		border-radius: 0.55rem;
		outline: 2px solid var(--color-library-manage);
		outline-offset: 0.35rem;
	}

	.management-editor-advanced {
		overflow: hidden;
		border: 1px solid color-mix(in oklab, var(--color-base-content) 10%, transparent);
		border-radius: 0.75rem;
		background: color-mix(in oklab, var(--color-base-200) 38%, transparent);
	}

	.management-editor-advanced > summary {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		padding: 0.8rem 0.9rem;
		list-style: none;
		cursor: pointer;
	}

	.management-editor-advanced > summary::-webkit-details-marker {
		display: none;
	}

	.management-editor-advanced > summary > :global(svg) {
		flex: none;
		color: color-mix(in oklab, var(--color-base-content) 42%, transparent);
		transition: transform 160ms ease;
	}

	.management-editor-advanced[open] > summary {
		border-bottom: 1px solid color-mix(in oklab, var(--color-base-content) 8%, transparent);
	}

	.management-editor-advanced[open] > summary > :global(svg) {
		transform: rotate(90deg);
	}

	.management-editor-advanced > :not(summary) {
		margin-inline: 0.9rem;
		margin-bottom: 0.9rem;
	}
</style>
