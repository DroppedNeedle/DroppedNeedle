<script lang="ts">
	import { resolve } from '$app/paths';
	import type { SourcePlaylistSummary } from '$lib/types';
	import type { Snippet } from 'svelte';
	import {
		ArrowRight,
		CheckCircle2,
		CircleAlert,
		Disc3,
		Link2,
		ListMusic,
		RefreshCw
	} from 'lucide-svelte';
	import { reveal } from '$lib/actions/reveal';

	interface Props {
		playlists?: SourcePlaylistSummary[];
		sourceLabel: string;
		playlistsHref: string;
		sourceIcon: Snippet;
		accountMode?: 'linked' | 'shared';
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
			.map((playlist) => playlist.cover_url)
			.filter(Boolean)
	);
	let progressPct = $derived(totalCount > 0 ? (importedCount / totalCount) * 100 : 0);

	const RING_RADIUS = 28;
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

<section
	use:reveal
	role="group"
	class="relative overflow-hidden rounded-2xl border border-base-content/5 bg-base-200/30 p-5 backdrop-blur-md sm:p-6"
	aria-live="polite"
	onpointerenter={() => (isHovered = true)}
	onpointerleave={() => (isHovered = false)}
>
	<div
		class="pointer-events-none absolute inset-0 bg-gradient-to-br from-primary/[0.04] via-transparent to-secondary/[0.04]"
	></div>

	<div class="relative flex items-center gap-5">
		<div class="hidden h-28 w-36 shrink-0 items-center justify-center sm:flex" aria-hidden="true">
			{#if !loading && !hasError && coverUrls.length > 0}
				{#each coverUrls as url, index (url)}
					<div
						class="absolute h-20 w-20 overflow-hidden rounded-xl border-2 border-base-100 shadow-md transition-transform duration-500"
						style="transform: rotate({isHovered
							? fanAngles[index] * 1.4
							: fanAngles[index]}deg) translateX({isHovered
							? fanOffsets[index] * 1.3
							: fanOffsets[index]}px); z-index: {index};"
					>
						<img src={url} alt="" class="h-full w-full object-cover" loading="lazy" />
					</div>
				{/each}
			{:else}
				<div
					class="flex h-20 w-20 items-center justify-center rounded-2xl border border-base-content/10 bg-base-100/70 shadow-sm"
				>
					{#if hasError}
						<CircleAlert class="h-8 w-8 text-warning" />
					{:else if totalCount === 0 && !loading}
						<ListMusic class="h-8 w-8 text-base-content/30" />
					{:else}
						<Disc3 class="h-8 w-8 text-base-content/20" />
					{/if}
				</div>
			{/if}
		</div>

		<div class="min-w-0 flex-1">
			<div class="mb-1.5 flex flex-wrap items-center gap-2">
				{@render sourceIcon()}
				<span class="text-xs font-medium uppercase tracking-wider text-base-content/50"
					>{sourceLabel}</span
				>
				{#if !loading && !hasError}
					<span class="badge badge-ghost badge-sm">
						{accountMode === 'linked' ? accountLabel : 'Shared account'}
					</span>
				{/if}
			</div>

			{#if loading}
				<div class="space-y-2 py-1">
					<div class="skeleton h-6 w-64 max-w-full"></div>
					<div class="skeleton h-4 w-80 max-w-full"></div>
				</div>
			{:else}
				<h3 class="text-lg font-bold leading-tight sm:text-xl">{heading}</h3>
				<p class="mt-1 max-w-2xl text-sm text-base-content/55">{supportingCopy}</p>
			{/if}

			{#if !loading}
				<div class="mt-4 flex flex-wrap items-center gap-2">
					{#if !hasError && totalCount > 0}
						<a class="btn btn-primary btn-sm gap-2" href={playlistsHref}>
							{allImported ? 'View playlists' : 'Browse playlists'}
							<ArrowRight class="h-4 w-4" />
						</a>
					{/if}
					{#if (!hasError && (accountMode === 'shared' || totalCount === 0)) || relinkRequired}
						<a class="btn btn-ghost btn-sm gap-2" href={resolve('/profile#media-accounts')}>
							<Link2 class="h-4 w-4" />
							{relinkRequired
								? `Reconnect ${sourceLabel}`
								: accountMode === 'linked'
									? 'Manage account'
									: 'Link your account'}
						</a>
					{/if}
					{#if onretry && (hasError || totalCount === 0)}
						<button type="button" class="btn btn-ghost btn-sm gap-2" onclick={onretry}>
							<RefreshCw class="h-4 w-4" />
							Check again
						</button>
					{/if}
				</div>
			{/if}
		</div>

		{#if !loading && !hasError && totalCount > 0}
			<div class="relative hidden shrink-0 sm:block">
				<svg class="h-[72px] w-[72px] -rotate-90" viewBox="0 0 64 64" fill="none">
					<circle
						cx="32"
						cy="32"
						r={RING_RADIUS}
						stroke="currentColor"
						stroke-width="4"
						class="text-base-content/10"
					/>
					<circle
						cx="32"
						cy="32"
						r={RING_RADIUS}
						stroke="currentColor"
						stroke-width="4"
						stroke-linecap="round"
						stroke-dasharray={RING_CIRCUMFERENCE}
						stroke-dashoffset={strokeDashoffset}
						class={allImported ? 'text-success' : 'text-primary'}
					/>
				</svg>
				<div class="absolute inset-0 flex items-center justify-center">
					{#if allImported}
						<CheckCircle2 class="h-6 w-6 text-success" />
					{:else}
						<span class="text-xs font-bold tabular-nums text-base-content/70">
							{importedCount}/{totalCount}
						</span>
					{/if}
				</div>
			</div>
		{/if}
	</div>
</section>
