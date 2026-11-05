<!--
	The hub's header: the server's icon in a round badge of its brand colour,
	"<Server> Library", and under it the album count with the refresh button.
	A server that is not set up says so in their place.
-->
<script lang="ts">
	import { RefreshCw } from 'lucide-svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import ListPageHeader from '$lib/ui/ListPageHeader.svelte';
	import type { RemoteSourceDescriptor } from '$lib/features/remote-library/queries/remoteSources';

	interface Props {
		remote: RemoteSourceDescriptor;
		albumCount: number | null;
		/** The server is not set up: no count, nothing to refresh. */
		off?: boolean;
		onrefresh: () => void;
		refreshing?: boolean;
	}

	let { remote, albumCount, off = false, onrefresh, refreshing = false }: Props = $props();
</script>

<ListPageHeader
	title="{remote.label} Library"
	ticket="DN / CC 3.2"
	fitTitle
	subtitle={off ? `${remote.label} isn't set up.` : undefined}
>
	{#snippet mark()}
		<span class="hub-badge">
			<remote.icon class="hub-badge__icon" />
		</span>
	{/snippet}
	{#snippet below()}
		{#if !off}
			<div class="flex items-center gap-3">
				{#if albumCount !== null}
					<span class="hub-count">
						{albumCount.toLocaleString()} album{albumCount === 1 ? '' : 's'}
					</span>
				{:else}
					<span class="skeleton skeleton-shimmer h-9 w-36 rounded-full"></span>
				{/if}
				<span class="hub-refresh"
					><IconButton
						variant="outline"
						shape="square"
						size="md"
						icon={RefreshCw}
						label="Refresh page data"
						loading={refreshing}
						onclick={onrefresh}
					/></span
				>
			</div>
		{/if}
	{/snippet}
</ListPageHeader>

<style>
	.hub-badge {
		display: grid;
		place-items: center;
		width: 5.5rem;
		height: 5.5rem;
		margin-top: 0.5rem;
		border: 3px solid color-mix(in oklab, var(--hub-brand) 55%, var(--color-base-100));
		border-radius: 999px;
		background: radial-gradient(
			circle at 35% 30%,
			color-mix(in oklab, var(--hub-brand) 70%, white),
			var(--hub-brand) 70%
		);
		box-shadow:
			0 0 0 5px var(--color-base-100),
			0 0 0 6px color-mix(in oklab, var(--hub-brand) 45%, transparent);
		color: var(--hub-brand-ink);
	}
	.hub-badge :global(.hub-badge__icon) {
		width: 2.75rem;
		height: 2.75rem;
	}

	.hub-count {
		display: inline-flex;
		align-items: center;
		height: 2.25rem;
		padding-inline: 1.125rem;
		border: 1px solid var(--dn-paper-border);
		border-radius: 999px;
		background: var(--dn-paper);
		color: var(--dn-paper-content);
		font-family: var(--font-mono);
		font-size: 1.0625rem;
		font-weight: 700;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		white-space: nowrap;
		font-variant-numeric: tabular-nums;
	}

	@media (max-width: 1399px) {
		.hub-badge {
			width: 4.5rem;
			height: 4.5rem;
		}
		.hub-badge :global(.hub-badge__icon) {
			width: 2.25rem;
			height: 2.25rem;
		}
	}

	@media (max-width: 767px) {
		.hub-badge {
			width: 2.75rem;
			height: 2.75rem;
			margin-top: 0;
			border-width: 2px;
			box-shadow: none;
		}
		.hub-badge :global(.hub-badge__icon) {
			width: 1.5rem;
			height: 1.5rem;
		}
		.hub-refresh :global(.btn) {
			--size: 2.25rem;
		}
		.hub-count {
			height: 1.875rem;
			padding-inline: 0.75rem;
			font-size: 0.75rem;
		}
	}

	@media (prefers-contrast: more) {
		.hub-badge {
			background: var(--color-base-100);
			color: var(--color-base-content);
			border-color: currentColor;
		}
	}
</style>
