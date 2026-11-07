<script lang="ts">
	import { fade, fly } from '$lib/ui/motion';
	import { Check, CircleCheck, TrendingUp } from 'lucide-svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import { QUALITY_TIERS } from '$lib/features/settings/components/qualityTiers';
	import {
		getCutoffUnmetQuery,
		createRequestUpgradeAlbumMutation
	} from '$lib/features/downloads/queries/UpgradeQueries.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { toastStore } from '$lib/stores/toast';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import RequestRow from './RequestRow.svelte';
	import RequestsAlert from './RequestsAlert.svelte';
	import RequestsEmpty from './RequestsEmpty.svelte';
	import RequestSkeleton from './RequestSkeleton.svelte';

	interface Props {
		active: boolean;
		oncount?: (count: number) => void;
	}

	let { active, oncount }: Props = $props();

	// Cutoff-unmet worklist for admin and trusted curators.
	const cutoffUnmetQuery = getCutoffUnmetQuery(() => authStore.isTrusted && active);
	const upgradeItems = $derived(cutoffUnmetQuery.data?.items ?? []);
	const upgradeAlbum = createRequestUpgradeAlbumMutation();
	// albums this visit already queued an upgrade for (button flips to "Queued")
	let upgradeQueued = $state<Set<string>>(new Set());

	$effect(() => oncount?.(upgradeItems.length));

	function tierLabel(key: string): string {
		return QUALITY_TIERS.find((t) => t.key === key)?.full ?? key;
	}

	async function handleUpgrade(item: (typeof upgradeItems)[number]) {
		try {
			const result = await upgradeAlbum.mutateAsync({
				release_group_mbid: item.release_group_mbid,
				artist_name: item.artist_name ?? 'Unknown',
				album_title: item.album_title ?? 'Unknown',
				year: item.year,
				artist_mbid: item.artist_mbid
			});
			if (result.status === 'queued') {
				upgradeQueued = new Set([...upgradeQueued, item.release_group_mbid]);
				toastStore.show({
					message: `Looking for a better copy of ${item.album_title ?? 'this album'}`,
					type: 'success'
				});
			} else {
				toastStore.show({ message: 'Already at or above the cutoff', type: 'info' });
			}
		} catch (e) {
			toastStore.show({
				message: e instanceof Error ? e.message : "Couldn't start that upgrade",
				type: 'error'
			});
		}
	}
</script>

{#if active}
	<div in:fade={{ duration: 150 }}>
		{#if cutoffUnmetQuery.isError}
			<RequestsAlert
				message="Could not load the upgrade worklist."
				onretry={() => void cutoffUnmetQuery.refetch()}
			/>
		{:else if cutoffUnmetQuery.isPending}
			<RequestSkeleton count={3} />
		{:else if !cutoffUnmetQuery.data?.upgrade_allowed}
			<RequestsEmpty icon={TrendingUp} tone="muted" title="Upgrades are off">
				{#if authStore.isAdmin}
					Turn on "Allow automatic upgrades" in Settings > Download Client to list albums below your
					quality cutoff.
				{:else}
					Ask an admin to turn on automatic upgrades.
				{/if}
				{#snippet action()}
					{#if authStore.isAdmin}
						<Button
							variant="primary"
							size="display-sm"
							href={withBasePath('/settings?tab=download-client')}
							class="mt-5"
						>
							Open download settings
						</Button>
					{/if}
				{/snippet}
			</RequestsEmpty>
		{:else if upgradeItems.length === 0}
			<RequestsEmpty icon={CircleCheck} tone="success" title="Everything meets your cutoff">
				No album is below {tierLabel(cutoffUnmetQuery.data.cutoff)}. Albums that fall short will
				appear here.
			</RequestsEmpty>
		{:else}
			{@const cutoff = tierLabel(cutoffUnmetQuery.data.cutoff)}
			<p class="rq-intro mb-5">
				Albums whose worst track is below your cutoff ({cutoff}). "Find a better copy" only replaces
				a file when the new one is better quality; replaced files go to the recycle bin.
			</p>
			<div class="rq-upgrades">
				{#each upgradeItems as item, i (item.release_group_mbid)}
					{@const queued = upgradeQueued.has(item.release_group_mbid)}
					<div in:fly={{ y: 12, duration: 200, delay: Math.min(i, 12) * 30 }}>
						<RequestRow>
							{#snippet cover()}
								<AlbumImage
									mbid={item.release_group_mbid}
									customUrl={null}
									alt={item.album_title ?? 'Album'}
									size="sm"
									rounded="none"
									className="w-full h-full"
								/>
							{/snippet}
							{#snippet aside()}
								<IconButton
									variant={queued ? 'ghost' : 'outline'}
									size="display"
									shape="square"
									icon={queued ? Check : TrendingUp}
									label={queued ? 'Queued' : 'Find a better copy'}
									disabled={queued || upgradeAlbum.isPending}
									onclick={() => void handleUpgrade(item)}
								/>
							{/snippet}
							<a
								href={withBasePath(`/album/${item.release_group_mbid}`)}
								class="rq-title rq-title--sm"
							>
								{item.album_title ?? 'Unknown album'}
							</a>
							<p class="truncate text-sm font-semibold">
								{item.artist_name ?? 'Unknown artist'}{item.year ? ` • ${item.year}` : ''}
							</p>
							<p class="rq-meta mt-1.5">
								<span>
									<span class="font-semibold text-warning">{tierLabel(item.current_tier)}</span>
									→ {cutoff}
								</span>
								<span>{item.track_count} tracks</span>
							</p>
						</RequestRow>
					</div>
				{/each}
			</div>
		{/if}
	</div>
{/if}

<style>
	/* a long worklist: two columns of compact rows on a wide screen */
	.rq-upgrades {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(min(100%, 34rem), 1fr));
		gap: 1rem;
	}
	.rq-upgrades :global(.rq-row__grid) {
		gap: 0 1.25rem;
		padding: 1rem 1.25rem;
	}
	.rq-upgrades :global(.rq-row__cover) {
		width: 4.5rem;
		height: 4.5rem;
		border-radius: 0.625rem;
	}
	.rq-upgrades :global(.rq-row__aside) {
		align-self: center;
	}
	.rq-upgrades :global(.rq-title--sm) {
		font-size: 1.375rem;
		-webkit-line-clamp: 1;
		line-clamp: 1;
	}
	@media (max-width: 767px) {
		.rq-upgrades :global(.rq-row__grid) {
			grid-template-columns: auto minmax(0, 1fr) auto;
			grid-template-areas: 'cover body aside';
			padding: 0.875rem;
			gap: 0 0.875rem;
		}
		.rq-upgrades :global(.rq-row__cover) {
			width: 4rem;
			height: 4rem;
		}
		.rq-upgrades :global(.rq-title--sm) {
			font-size: 1.125rem;
		}
	}
</style>
