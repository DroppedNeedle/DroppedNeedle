<script lang="ts">
	import type {
		RemoteAccountMode,
		RemotePlaylist
	} from '$lib/features/remote-library/queries/types';
	import type { Snippet } from 'svelte';
	import {
		ArrowRight,
		CircleCheckBig,
		CircleAlert,
		Link2,
		ListMusic,
		RefreshCw
	} from 'lucide-svelte';
	import CoverPlaceholder from '$lib/ui/CoverPlaceholder.svelte';
	import { reveal } from '$lib/actions/reveal';
	import FritEdge from '$lib/ui/FritEdge.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import Button from '$lib/ui/Button.svelte';

	interface Props {
		playlists?: RemotePlaylist[];
		sourceLabel: string;
		playlistsHref: string;
		sourceIcon: Snippet;
		accountMode?: RemoteAccountMode;
		accountLabel?: string;
		loading?: boolean;
		errorCode?: string;
		onretry?: () => void;
	}

	let {
		playlists = [],
		sourceLabel,
		playlistsHref,
		sourceIcon,
		accountMode = 'shared',
		accountLabel = `Shared ${sourceLabel} account`,
		loading = false,
		errorCode = '',
		onretry
	}: Props = $props();

	let totalCount = $derived(playlists.length);
	let importedCount = $derived(playlists.filter((playlist) => playlist.is_imported).length);
	let allImported = $derived(totalCount > 0 && importedCount === totalCount);
	let relinkRequired = $derived(errorCode === 'MEDIA_ACCOUNT_RELINK_REQUIRED');
	let hasError = $derived(Boolean(errorCode));
	let coverUrls = $derived(
		playlists
			.slice(0, 4)
			.map((playlist) => playlist.image_url)
			.filter((url): url is string => Boolean(url))
	);
	let progressPct = $derived(totalCount > 0 ? (importedCount / totalCount) * 100 : 0);

	const RING_RADIUS = 27;
	const RING_CIRCUMFERENCE = 2 * Math.PI * RING_RADIUS;
	let strokeDashoffset = $derived(RING_CIRCUMFERENCE - (progressPct / 100) * RING_CIRCUMFERENCE);
	const fanAngles = [-8, -3, 3, 8];
	const fanOffsets = [-12, -4, 4, 12];
	let isHovered = $state(false);

	let heading = $derived.by(() => {
		if (loading) return `Checking ${sourceLabel} playlists`;
		if (relinkRequired) return `Reconnect ${sourceLabel} to check your playlists`;
		if (hasError) return `Couldn't check ${sourceLabel} playlists`;
		if (accountMode === 'shared' && totalCount > 0) {
			return `Playlists from the shared ${sourceLabel} account`;
		}
		if (accountMode === 'shared') return `No playlists found on the shared ${sourceLabel} account`;
		if (totalCount === 0) return `No ${sourceLabel} playlists found for ${accountLabel}`;
		if (allImported)
			return `All ${totalCount} ${sourceLabel} playlist${totalCount === 1 ? '' : 's'} imported`;
		return `Bring your ${totalCount} ${sourceLabel} playlist${totalCount === 1 ? '' : 's'} to DroppedNeedle`;
	});

	let supportingCopy = $derived.by(() => {
		if (loading) return 'Checking for playlists you can access.';
		if (relinkRequired)
			return `Reconnect your ${sourceLabel} account so DroppedNeedle can check its playlists.`;
		if (hasError) return "We couldn't load playlists from this server. Your library is unaffected.";
		if (accountMode === 'shared') return 'Link your account to see your playlists.';
		if (totalCount === 0) return `${accountLabel} is connected, but no playlists are available.`;
		if (allImported)
			return `Your ${sourceLabel} playlists are now private copies in DroppedNeedle.`;
		if (importedCount === 0) return 'Choose the playlists you want to import.';
		return `${importedCount} of ${totalCount} imported so far.`;
	});
</script>

<div use:reveal class="pib-wrap">
	<section
		class="pib"
		aria-live="polite"
		aria-label="Playlist import"
		onpointerenter={() => (isHovered = true)}
		onpointerleave={() => (isHovered = false)}
	>
		<FritEdge class="pib__frit" />
		<p class="pib__tag" aria-hidden="true"><span>Playlist import</span></p>

		<div class="pib__fan" aria-hidden="true">
			{#if !loading && !hasError && coverUrls.length > 0}
				{#each coverUrls as url, index (url)}
					<div
						class="pib__cover"
						style:transform="rotate({isHovered ? fanAngles[index] * 1.4 : fanAngles[index]}deg)
						translateX({isHovered ? fanOffsets[index] * 1.3 : fanOffsets[index]}px)"
						style:z-index={index}
					>
						<AlbumImage
							customUrl={url}
							alt=""
							size="full"
							rounded="none"
							className="h-full w-full"
						/>
					</div>
				{/each}
			{:else}
				<div class="pib__cover pib__cover--empty">
					{#if hasError}
						<CircleAlert class="size-8 text-warning" />
					{:else if totalCount === 0 && !loading}
						<ListMusic class="size-8 text-muted" />
					{:else}
						<CoverPlaceholder />
					{/if}
				</div>
			{/if}
		</div>

		<div class="pib__body">
			<div class="pib__source">
				{@render sourceIcon()}
				<span>{sourceLabel}</span>
				{#if !loading && !hasError}
					<StatusChip size="xs">
						{accountMode === 'linked' ? accountLabel : 'Shared account'}
					</StatusChip>
				{/if}
			</div>
			{#if loading}
				<div class="space-y-2 py-1">
					<div class="skeleton h-5 w-64 max-w-full"></div>
					<div class="skeleton h-4 w-80 max-w-full"></div>
				</div>
			{:else}
				<h3 class="pib__heading">{heading}</h3>
				<p class="pib__copy">{supportingCopy}</p>
			{/if}
		</div>

		{#if !loading}
			<div class="pib__actions">
				{#if !hasError && totalCount > 0}
					<Button
						variant="primary"
						size="display-sm"
						icon={ArrowRight}
						iconPosition="end"
						href={playlistsHref}
					>
						{allImported ? 'View playlists' : 'Browse playlists'}
					</Button>
				{/if}
				{#if (!hasError && (accountMode === 'shared' || totalCount === 0)) || relinkRequired}
					<Button
						variant="outline"
						size="display-sm"
						icon={Link2}
						href={withBasePath('/profile#media-accounts')}
					>
						{relinkRequired
							? `Reconnect ${sourceLabel}`
							: accountMode === 'linked'
								? 'Manage account'
								: 'Link your account'}
					</Button>
				{/if}
				{#if onretry && (hasError || totalCount === 0)}
					<Button variant="outline" size="display-sm" icon={RefreshCw} onclick={onretry}>
						Check again
					</Button>
				{/if}
			</div>
		{/if}

		{#if !loading && !hasError && totalCount > 0}
			<div class="pib__ring">
				<svg class="size-full -rotate-90" viewBox="0 0 64 64" fill="none" aria-hidden="true">
					<circle cx="32" cy="32" r={RING_RADIUS} stroke-width="5" class="pib__ring-track" />
					<circle
						cx="32"
						cy="32"
						r={RING_RADIUS}
						stroke-width="5"
						stroke-linecap="round"
						stroke-dasharray={RING_CIRCUMFERENCE}
						stroke-dashoffset={strokeDashoffset}
						class={allImported ? 'pib__ring-done' : 'pib__ring-fill'}
					/>
				</svg>
				<div class="absolute inset-0 grid place-items-center">
					{#if allImported}
						<CircleCheckBig class="size-6 text-success" aria-label="All imported" />
					{:else}
						<span class="pib__ring-count">{importedCount}/{totalCount}</span>
					{/if}
				</div>
			</div>
		{/if}
	</section>
</div>

<style>
	.pib-wrap {
		container: pib / inline-size;
	}

	.pib {
		position: relative;
		isolation: isolate;
		display: flex;
		align-items: center;
		gap: 1.25rem;
		min-height: 100%;
		padding: 1rem 1.25rem 1rem 1rem;
		overflow: hidden;
		border: 1px solid var(--dn-line);
		border-radius: var(--radius-box);
		background: var(--color-base-200);
	}
	.pib :global(.pib__frit) {
		z-index: -1;
		top: 0;
		right: 0;
		width: 16rem;
		height: 9rem;
	}

	.pib__tag {
		flex: none;
		padding: 0.5rem 1.25rem 0.375rem;
		background: var(--dn-action);
		color: var(--dn-action-content);
		transform: rotate(-3deg) skewX(-6deg);
		box-shadow: 3px 3px 0 color-mix(in oklab, black 45%, transparent);
	}
	.pib__tag span {
		display: block;
		font-family: var(--font-display);
		font-size: 2rem;
		line-height: 1;
		text-transform: uppercase;
		white-space: nowrap;
		font-synthesis: none;
		transform: skewX(6deg);
	}

	.pib__fan {
		position: relative;
		display: none;
		flex: none;
		place-items: center;
		width: 7.5rem;
		height: 5.5rem;
	}
	.pib__cover {
		position: absolute;
		width: 4.5rem;
		height: 4.5rem;
		overflow: hidden;
		border: 2px solid var(--dn-paper);
		border-radius: 0.25rem;
		box-shadow: 0 4px 12px color-mix(in oklab, black 40%, transparent);
		transition: transform 0.5s var(--ease-spring);
	}
	.pib__cover--empty {
		position: relative;
		display: grid;
		place-items: center;
		border-color: var(--dn-line);
		background: var(--color-base-100);
		box-shadow: none;
	}

	.pib__body {
		min-width: 0;
		flex: 1;
	}
	.pib__source {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.5rem;
		margin-bottom: 0.25rem;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 700;
		letter-spacing: 0.16em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.pib__heading {
		font-size: 1.0625rem;
		font-weight: 700;
		line-height: 1.25;
	}
	.pib__copy {
		margin-top: 0.125rem;
		font-size: 0.875rem;
		color: var(--dn-muted);
	}

	.pib__actions {
		display: flex;
		flex: none;
		flex-wrap: wrap;
		gap: 0.5rem;
	}

	.pib__ring {
		position: relative;
		flex: none;
		width: 4rem;
		height: 4rem;
	}
	.pib__ring-track {
		stroke: var(--dn-line);
	}
	.pib__ring-fill {
		stroke: var(--dn-title);
	}
	.pib__ring-done {
		stroke: var(--color-success);
	}
	.pib__ring-count {
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 700;
		font-variant-numeric: tabular-nums;
	}

	/* a full-width band has room for the covers */
	@container pib (min-width: 62rem) {
		.pib__fan {
			display: grid;
		}
	}

	/* paired beside the counts: the tag over the words, the buttons under
	   them, the ring at the right */
	@container pib (max-width: 46rem) {
		.pib {
			display: grid;
			grid-template-columns: minmax(0, 1fr) auto;
			align-content: center;
			gap: 0.625rem 1rem;
			padding: 1.125rem 1.25rem;
		}
		.pib__tag {
			justify-self: start;
			padding: 0.375rem 1rem 0.25rem;
		}
		.pib__tag span {
			font-size: 1.5rem;
		}
		.pib__body {
			grid-column: 1;
		}
		.pib__actions {
			grid-column: 1;
		}
		.pib__ring {
			grid-column: 2;
			grid-row: 1 / 4;
			align-self: center;
		}
	}

	@container pib (max-width: 30rem) {
		.pib__tag {
			display: none;
		}
		.pib__ring {
			display: none;
		}
		.pib__body {
			flex-basis: 100%;
		}
		.pib__actions :global(.btn) {
			flex: 1 1 auto;
		}
	}
</style>
