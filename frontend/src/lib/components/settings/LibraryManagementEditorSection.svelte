<!--
	One collapsible section of the management profile editor: icon, title,
	an On/Off state and summary chips, then the section's controls.
-->
<script lang="ts">
	import '$lib/components/library/library-management.css';
	import type { Snippet } from 'svelte';
	import { ChevronRight } from 'lucide-svelte';
	import type { KitIcon } from '$lib/ui/Button.svelte';

	interface Chip {
		label: string;
		active?: boolean;
	}

	interface Props {
		icon: KitIcon;
		title: string;
		subtitle: string;
		active: boolean;
		/** Replaces the On/Off state text. */
		state?: string;
		chips: Chip[];
		children: Snippet;
	}

	let { icon: Icon, title, subtitle, active, state, chips, children }: Props = $props();
</script>

<details class="management-editor-section" data-active={active}>
	<summary class="management-editor-summary">
		<span class="management-editor-icon"><Icon class="h-4 w-4" /></span>
		<span class="management-editor-summary__copy"
			><strong>{title}</strong><small>{subtitle}</small></span
		>
		<span class="management-editor-overview">
			<span class="management-editor-state" data-active={active}
				>{state ?? (active ? 'On' : 'Off')}</span
			>
			{#each chips as chip (chip.label)}
				<span class="management-editor-chip" data-active={chip.active}>{chip.label}</span>
			{/each}
		</span>
		<ChevronRight class="h-4 w-4 management-editor-chevron" />
	</summary>
	{@render children()}
</details>

<style>
	.management-editor-summary__copy {
		display: grid;
		gap: 0.12rem;
	}

	.management-editor-summary small {
		color: color-mix(in oklab, var(--color-base-content) 52%, transparent);
		font-size: 0.7rem;
		font-weight: 400;
	}

	.management-editor-summary {
		align-items: center;
		gap: 0.7rem;
		list-style: none;
		cursor: pointer;
	}

	.management-editor-summary {
		display: grid;
		grid-template-columns: auto minmax(11rem, 1fr) minmax(0, auto) auto;
	}

	.management-editor-summary:focus-visible {
		border-radius: 0.55rem;
		outline: 2px solid var(--color-library-manage);
		outline-offset: 0.35rem;
	}

	.management-editor-summary::-webkit-details-marker {
		display: none;
	}

	.management-editor-overview {
		display: flex;
		flex-wrap: wrap;
		justify-content: flex-end;
		gap: 0.35rem;
	}

	.management-editor-chip {
		display: inline-flex;
		align-items: center;
		min-height: 1.55rem;
		padding-inline: 0.55rem;
		border: 1px solid color-mix(in oklab, var(--color-base-content) 10%, transparent);
		border-radius: 9999px;
		background: color-mix(in oklab, var(--color-base-200) 65%, transparent);
		color: color-mix(in oklab, var(--color-base-content) 52%, transparent);
		font-size: 0.65rem;
		font-weight: 650;
		line-height: 1.1;
	}

	.management-editor-chip[data-active='true'] {
		border-color: color-mix(in oklab, var(--color-library-manage) 26%, transparent);
		background: color-mix(in oklab, var(--color-library-manage) 9%, var(--color-base-100));
		color: color-mix(in oklab, var(--color-library-manage) 67%, var(--color-base-content));
	}

	@media (max-width: 767px) {
		.management-editor-summary {
			grid-template-columns: auto minmax(0, 1fr) auto;
		}

		.management-editor-overview {
			grid-column: 2 / -1;
			justify-content: flex-start;
		}
	}
</style>
