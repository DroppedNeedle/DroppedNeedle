<script lang="ts">
	import { ChevronDown } from 'lucide-svelte';
	import type { Snippet } from 'svelte';
	import Card from '$lib/ui/Card.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	interface Props {
		title: string;
		sourceLabel: string;
		icon: typeof ChevronDown;
		connected: boolean;
		statusText: string;
		enabled: boolean;
		enableAriaLabel: string;
		onToggle?: () => void;
		children: Snippet;
	}

	let {
		title,
		sourceLabel,
		icon: Icon,
		connected,
		statusText,
		enabled = $bindable(),
		enableAriaLabel,
		onToggle,
		children
	}: Props = $props();

	let open = $state(false);
</script>

<Card variant="bordered" padding="none" class="client-card {enabled ? 'is-active' : ''}">
	<div>
		<div class="flex flex-wrap items-center gap-4 p-5">
			<div class="grid size-12 place-items-center rounded-2xl bg-base-300/60">
				<Icon class="size-6 text-accent" aria-hidden="true" />
			</div>
			<button
				type="button"
				class="min-w-0 flex-1 text-left"
				onclick={() => (open = !open)}
				aria-expanded={open}
			>
				<div class="flex items-center gap-2">
					<h3 class="text-lg font-bold">{title}</h3>
					<span class="badge badge-ghost badge-sm">{sourceLabel}</span>
				</div>
				<div class="flex items-center gap-2 text-sm text-base-content/70">
					<span
						class="orb"
						class:is-connected={connected}
						role="status"
						aria-label={connected ? 'Connected' : 'Not connected'}
					></span>
					{statusText}
				</div>
			</button>
			<label class="flex cursor-pointer items-center gap-2">
				<span class="text-sm font-medium">{enabled ? 'Enabled' : 'Disabled'}</span>
				<input
					type="checkbox"
					class="toggle toggle-sm toggle-primary"
					bind:checked={enabled}
					onchange={() => onToggle?.()}
					aria-label={enableAriaLabel}
				/>
			</label>
			<IconButton
				label={open ? 'Collapse' : 'Expand'}
				icon={ChevronDown}
				size="sm"
				shape="square"
				class={open ? 'rotate-180' : ''}
				aria-expanded={open}
				onclick={() => (open = !open)}
			/>
		</div>

		{#if open}
			<div class="space-y-5 border-t border-base-300 p-5">
				{@render children()}
			</div>
		{/if}
	</div>
</Card>

<style>
	:global(.client-card) {
		transition:
			box-shadow 0.4s ease,
			border-color 0.4s ease;
	}
	:global(.client-card.is-active) {
		border-color: oklch(from var(--color-accent) l c h / 0.55);
		box-shadow:
			0 0 0 1px oklch(from var(--color-accent) l c h / 0.3),
			0 0 44px oklch(from var(--color-accent) l c h / 0.18);
	}
	.orb {
		display: inline-block;
		width: 0.7rem;
		height: 0.7rem;
		border-radius: 9999px;
		background: oklch(from var(--color-base-content) l c h / 0.3);
		transition: background 0.3s ease;
	}
	.orb.is-connected {
		background: var(--color-accent);
		animation: orb-pulse 2.4s ease-in-out infinite;
	}
	@keyframes orb-pulse {
		0%,
		100% {
			box-shadow: 0 0 5px oklch(from var(--color-accent) l c h / 0.5);
		}
		50% {
			box-shadow: 0 0 14px oklch(from var(--color-accent) l c h / 0.95);
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.orb.is-connected {
			animation: none;
		}
	}
</style>
