<script lang="ts">
	import { ChevronDown } from 'lucide-svelte';
	import type { Snippet } from 'svelte';
	import Card from '$lib/ui/Card.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';

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
		<div class="flex flex-wrap items-center gap-x-4 gap-y-3 p-4 sm:p-5">
			<div class="client-card__mark">
				<Icon class="size-6" aria-hidden="true" />
			</div>
			<button
				type="button"
				class="min-w-0 flex-1 basis-40 text-left"
				onclick={() => (open = !open)}
				aria-expanded={open}
			>
				<span class="flex flex-wrap items-center gap-x-2.5 gap-y-1">
					<h3 class="client-card__name">{title}</h3>
					<StatusChip tone="action" size="xs">{sourceLabel}</StatusChip>
				</span>
				<span class="mt-1 flex items-center gap-2 text-sm text-muted">
					<span
						class="orb"
						class:is-connected={connected}
						role="status"
						aria-label={connected ? 'Connected' : 'Not connected'}
					></span>
					{statusText}
				</span>
			</button>
			<label class="flex cursor-pointer items-center gap-2.5">
				<span class="text-sm font-semibold">{enabled ? 'Enabled' : 'Disabled'}</span>
				<input
					type="checkbox"
					class="toggle toggle-primary"
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
				class="transition-transform {open ? 'rotate-180' : ''}"
				aria-expanded={open}
				onclick={() => (open = !open)}
			/>
		</div>

		{#if open}
			<div class="space-y-5 border-t border-line p-4 sm:p-5">
				{@render children()}
			</div>
		{/if}
	</div>
</Card>

<style>
	:global(.client-card) {
		transition: border-color 0.3s ease;
	}
	:global(.client-card.is-active) {
		border-color: var(--dn-action-edge);
		box-shadow: inset 0 0 0 1px var(--dn-action-edge);
	}
	.client-card__mark {
		display: grid;
		flex: none;
		place-items: center;
		width: 3rem;
		height: 3rem;
		border: 1px solid var(--dn-line);
		border-radius: 0.75rem;
		background-color: var(--color-base-100);
		color: var(--dn-title);
	}
	.client-card__name {
		font-size: 1.25rem;
		font-weight: 800;
		line-height: 1.2;
	}
	.orb {
		display: inline-block;
		flex: none;
		width: 0.625rem;
		height: 0.625rem;
		border-radius: 9999px;
		background: color-mix(in oklab, var(--color-base-content) 30%, transparent);
		transition: background 0.3s ease;
	}
	.orb.is-connected {
		background: var(--color-success);
		animation: orb-pulse 2.4s ease-in-out infinite;
	}
	@keyframes orb-pulse {
		0%,
		100% {
			box-shadow: 0 0 0 2px color-mix(in oklab, var(--color-success) 25%, transparent);
		}
		50% {
			box-shadow: 0 0 0 5px color-mix(in oklab, var(--color-success) 12%, transparent);
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.orb.is-connected {
			animation: none;
		}
	}
</style>
