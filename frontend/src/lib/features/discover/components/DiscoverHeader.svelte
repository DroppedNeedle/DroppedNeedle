<!--
	Discover's header: the huge Anton title, a rule, what the page is for in
	short spaced mono lines, and at the right a big record with an acid
	label cut by the page's top and right edges, the barcode ticket tilted
	over it. "Refreshing" and the refresh button sit under the record. The
	record and ticket are decoration.
-->
<script lang="ts">
	import { RefreshCw } from 'lucide-svelte';
	import { formatLastUpdated } from '$lib/utils/formatting';
	import BarcodeTicket from '$lib/ui/BarcodeTicket.svelte';
	import Button from '$lib/ui/Button.svelte';
	import LiveUpdatingBadge from '$lib/ui/LiveUpdatingBadge.svelte';
	import VinylRecord from '$lib/ui/VinylRecord.svelte';

	interface Props {
		loading?: boolean;
		refreshing?: boolean;
		isUpdating?: boolean;
		lastUpdated?: Date | null;
		onRefresh: () => void;
	}

	let {
		loading = false,
		refreshing = false,
		isUpdating = false,
		lastUpdated = null,
		onRefresh
	}: Props = $props();

	const refreshLabel = 'Refresh recommendations';
</script>

<header class="dn-dh">
	<div class="dn-dh__deck" aria-hidden="true">
		<VinylRecord sheen="acid" label class="dn-dh__record" />
		<BarcodeTicket label="DN / CC 3.2" class="dn-dh__ticket" />
	</div>

	<div class="dn-dh__row">
		<h1 class="dn-dh__title">Discover</h1>
		<span class="dn-decor dn-dh__rule" aria-hidden="true"></span>
		<p class="dn-dh__lines">
			<span>Music</span>
			<span>recommendations</span>
			<span>based on what</span>
			<span>you listen to.</span>
		</p>
	</div>

	<div class="dn-dh__actions">
		{#if isUpdating}
			<LiveUpdatingBadge label="Refreshing" />
		{:else if lastUpdated && !loading}
			<span class="dn-dh__updated">Updated {formatLastUpdated(lastUpdated)}</span>
		{/if}
		<Button
			variant="outline"
			size="display-sm"
			onclick={onRefresh}
			disabled={refreshing || loading}
			title={refreshLabel}
			aria-label={refreshLabel}
		>
			<RefreshCw class="size-4 {refreshing ? 'motion-safe:animate-spin' : ''}" />
			<span class="dn-dh__refresh-label">{refreshLabel}</span>
		</Button>
	</div>
</header>

<style>
	.dn-dh {
		--pad-top: 1.5rem;
		position: relative;
		isolation: isolate;
		overflow: clip;
		display: flex;
		align-items: flex-end;
		justify-content: space-between;
		gap: 1.5rem;
		min-height: 15rem;
		padding: var(--pad-top) 2rem 1.75rem;
	}

	/* the record hangs off the top and right edges; the ticket sits on it */
	.dn-dh__deck {
		position: absolute;
		z-index: -1;
		top: 0;
		right: 0;
		width: 30rem;
		height: 100%;
		pointer-events: none;
	}
	.dn-dh__deck :global(.dn-dh__record) {
		--sheen-from: 150deg;
		position: absolute;
		top: -13rem;
		right: -7rem;
		width: 23rem;
	}
	.dn-dh__deck :global(.dn-dh__ticket) {
		position: absolute;
		top: 1.75rem;
		right: 15.5rem;
		width: 9.5rem;
		rotate: -6deg;
	}

	.dn-dh__row {
		display: flex;
		align-items: center;
		gap: 1.75rem;
		min-width: 0;
	}

	.dn-dh__title {
		font-family: var(--font-display);
		font-weight: 400;
		font-size: 8.5rem;
		line-height: 0.9;
		letter-spacing: 0.005em;
		text-transform: uppercase;
		color: var(--dn-title);
		font-synthesis: none;
	}

	.dn-dh__rule {
		flex: none;
		align-self: stretch;
		width: 2px;
		margin-block: 1rem 0.75rem;
		background-color: color-mix(in oklab, var(--dn-title) 70%, transparent);
	}

	.dn-dh__lines {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		font-family: var(--font-mono);
		font-size: 0.9375rem;
		font-weight: 600;
		line-height: 1.25;
		letter-spacing: 0.24em;
		text-transform: uppercase;
		color: var(--color-base-content);
	}

	.dn-dh__actions {
		display: flex;
		flex: none;
		flex-wrap: wrap;
		align-items: center;
		justify-content: flex-end;
		gap: 0.75rem;
	}
	.dn-dh__updated {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		letter-spacing: 0.12em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}

	@media (max-width: 1279px) {
		.dn-dh__title {
			font-size: 6.5rem;
		}
		.dn-dh__lines {
			font-size: 0.8125rem;
		}
	}

	@media (max-width: 1023px) {
		.dn-dh {
			flex-direction: column;
			align-items: flex-start;
			min-height: 0;
			padding-inline: 1.5rem;
		}
		.dn-dh__actions {
			justify-content: flex-start;
		}
		.dn-dh__deck :global(.dn-dh__ticket) {
			right: 12rem;
		}
	}

	/* Phone: the title and the lines stacked, the record peeking in at the
	   top right, the refresh button an icon beside the title. */
	@media (max-width: 767px) {
		.dn-dh {
			--pad-top: 1.25rem;
			display: grid;
			grid-template-columns: minmax(0, 1fr) auto;
			align-items: end;
			gap: 0.75rem;
			padding: var(--pad-top) 1rem 1rem;
		}
		.dn-dh__row {
			flex-direction: column;
			align-items: flex-start;
			gap: 0.5rem;
		}
		.dn-dh__title {
			font-size: clamp(3rem, 15vw, 4.25rem);
		}
		.dn-dh__rule {
			display: none;
		}
		.dn-dh__lines {
			display: block;
			max-width: 15rem;
			font-size: 0.6875rem;
			letter-spacing: 0.16em;
		}
		.dn-dh__lines span + span::before {
			content: ' ';
		}
		.dn-dh__actions {
			flex-direction: column;
			align-items: flex-end;
			gap: 0.5rem;
		}
		.dn-dh__updated {
			display: none;
		}
		.dn-dh__refresh-label {
			display: none;
		}
		.dn-dh__deck {
			width: 12rem;
		}
		.dn-dh__deck :global(.dn-dh__record) {
			top: -6.5rem;
			right: -4rem;
			width: 12rem;
		}
		.dn-dh__deck :global(.dn-dh__ticket) {
			top: 0.5rem;
			right: 2.25rem;
			width: 5.25rem;
		}
	}

	@media (max-width: 379px) {
		.dn-dh__deck :global(.dn-dh__ticket) {
			display: none;
		}
	}
</style>
