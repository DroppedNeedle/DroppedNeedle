<script lang="ts">
	import { fade, fly } from '$lib/ui/motion';
	import { Check, CircleCheck, TrendingUp, TriangleAlert } from 'lucide-svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import { QUALITY_TIERS } from '$lib/components/settings/qualityTiers';
	import {
		getCutoffUnmetQuery,
		requestUpgradeAlbum
	} from '$lib/queries/downloads/UpgradeQueries.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { toastStore } from '$lib/stores/toast';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import RequestsEmpty from './RequestsEmpty.svelte';

	interface Props {
		active: boolean;
		oncount?: (count: number) => void;
	}

	let { active, oncount }: Props = $props();

	// Cutoff-unmet worklist for admin and trusted curators.
	const cutoffUnmetQuery = getCutoffUnmetQuery(() => authStore.isTrusted && active);
	const upgradeItems = $derived(cutoffUnmetQuery.data?.items ?? []);
	const upgradeAlbum = requestUpgradeAlbum();
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
			<div class="alert alert-warning mb-4">
				<TriangleAlert class="h-5 w-5" />
				<span>Could not load the upgrade worklist.</span>
				<Button size="sm" onclick={() => void cutoffUnmetQuery.refetch()}>Retry</Button>
			</div>
		{:else if cutoffUnmetQuery.isPending}
			<div class="flex flex-col gap-2.5">
				{#each Array(3) as _, i (`upgrade-loading-${i}`)}
					<div
						class="flex items-center gap-3 sm:gap-4 p-3 sm:p-4 bg-base-200 rounded-box animate-pulse"
						style:animation-delay="{i * 100}ms"
					>
						<div class="w-14 h-14 sm:w-16 sm:h-16 bg-base-300 rounded-lg"></div>
						<div class="flex-1">
							<div class="h-4 bg-base-300 rounded w-44 mb-2"></div>
							<div class="h-3 bg-base-300 rounded w-28"></div>
						</div>
						<div class="h-8 bg-base-300 rounded-btn w-32"></div>
					</div>
				{/each}
			</div>
		{:else if !cutoffUnmetQuery.data?.upgrade_allowed}
			<RequestsEmpty icon={TrendingUp} tone="muted" title="Upgrades are off">
				Turn on "Allow automatic upgrades" in Settings → Download Clients to list albums below your
				quality cutoff.
				{#snippet action()}
					{#if authStore.isAdmin}
						<Button
							variant="primary"
							size="sm"
							href={withBasePath('/settings?tab=download-client')}
							class="mt-4"
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
			<p class="text-xs text-base-content/40 mb-3">
				Albums whose worst track is below your cutoff ({cutoff}). "Find a better copy" only replaces
				a file when the new one is better quality; replaced files go to the recycle bin.
			</p>
			<div class="flex flex-col gap-2.5">
				{#each upgradeItems as item, i (item.release_group_mbid)}
					{@const queued = upgradeQueued.has(item.release_group_mbid)}
					<div in:fly={{ y: 12, duration: 200, delay: i * 30 }}>
						<Card padding="none">
							<div class="flex items-center gap-3 sm:gap-4 p-3 sm:p-4">
								<div
									class="w-14 h-14 sm:w-16 sm:h-16 shrink-0 rounded-lg overflow-hidden bg-base-300"
								>
									<AlbumImage
										mbid={item.release_group_mbid}
										customUrl={null}
										alt={item.album_title ?? 'Album'}
										size="sm"
										rounded="lg"
										className="w-full h-full"
									/>
								</div>
								<div class="flex-1 min-w-0">
									<a
										href={withBasePath(`/album/${item.release_group_mbid}`)}
										class="block font-semibold text-sm truncate hover:text-accent hover:underline"
									>
										{item.album_title ?? 'Unknown album'}
									</a>
									<p class="text-base-content/60 text-xs truncate">
										{item.artist_name ?? 'Unknown artist'}{item.year ? ` • ${item.year}` : ''}
									</p>
									<p class="text-base-content/40 text-xs mt-0.5">
										<span class="text-warning/80">{tierLabel(item.current_tier)}</span>
										<span class="text-base-content/25"> → {cutoff}</span>
										<span class="text-base-content/25"> • {item.track_count} tracks</span>
									</p>
								</div>
								<div class="shrink-0">
									<Button
										variant={queued ? 'ghost' : 'outline'}
										size="sm"
										icon={queued ? Check : TrendingUp}
										class="gap-1.5"
										disabled={queued || upgradeAlbum.isPending}
										onclick={() => void handleUpgrade(item)}
									>
										{queued ? 'Queued' : 'Find a better copy'}
									</Button>
								</div>
							</div>
						</Card>
					</div>
				{/each}
			</div>
		{/if}
	</div>
{/if}
