<!--
	The top of a playlist page: the mosaic as a sleeve with the record out of
	its side, the name as large as it can wrap, mono lines for who can see
	it, its counts and its owner, then Play All, Shuffle and Suggest Albums.
	The "..." menu at the top right holds Edit name, the visibility switch
	and Delete playlist, each only where allowed. The name is the page's only
	h1.
-->
<script lang="ts">
	import type { PlaylistDetail } from '$lib/api/playlists';
	import {
		createDeletePlaylistCoverV3Mutation,
		createUpdatePlaylistV3Mutation,
		createUploadPlaylistCoverV3Mutation
	} from '$lib/features/playlists/queries/PlaylistV3Mutations.svelte';
	import { toPageDetail } from '$lib/features/playlists/queries/playlistV3Adapter';
	import PlaylistDiscoveryModal from '$lib/features/discover/components/PlaylistDiscoveryModal.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { formatTotalDurationSec, formatRelativeTime } from '$lib/utils/formatting';
	import { getSourceColor, getSourceLabel } from '$lib/utils/sources';
	import { withBasePath } from '$lib/utils/basePath';
	import PlaylistMosaic from '$lib/media/PlaylistMosaic.svelte';
	import ContextMenu from '$lib/ui/ContextMenu.svelte';
	import type { MenuItem } from '$lib/ui/ContextMenu.svelte';
	import {
		ArrowLeft,
		Play,
		Shuffle,
		Pencil,
		PencilLine,
		Check,
		X,
		Sparkles,
		Globe,
		Lock,
		Trash2
	} from 'lucide-svelte';
	import JellyfinIcon from '$lib/media/icons/JellyfinIcon.svelte';
	import NavidromeIcon from '$lib/media/icons/NavidromeIcon.svelte';
	import PlexIcon from '$lib/media/icons/PlexIcon.svelte';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Input from '$lib/ui/Input.svelte';
	import VinylSleeve from '$lib/ui/VinylSleeve.svelte';

	interface Props {
		playlist: PlaylistDetail;
		canEdit?: boolean;
		canDelete?: boolean;
		sharePending?: boolean;
		onplayall: () => void;
		onshuffleall: () => void;
		ondeleteclick: () => void;
		onplaylistupdate: (updated: PlaylistDetail) => void;
		onshare?: (isPublic: boolean) => void;
	}

	let {
		playlist,
		canEdit = false,
		canDelete = false,
		sharePending = false,
		onplayall,
		onshuffleall,
		ondeleteclick,
		onplaylistupdate,
		onshare
	}: Props = $props();

	let ownerInitials = $derived(
		(playlist.owner_name ?? '')
			.trim()
			.split(/\s+/)
			.map((w) => w[0])
			.slice(0, 2)
			.join('')
			.toUpperCase() || '?'
	);

	let sourceType = $derived(playlist.source_ref?.split(':')[0] ?? null);
	let sourceColor = $derived(sourceType ? getSourceColor(sourceType) : null);
	let sourceLabel = $derived(sourceType ? getSourceLabel(sourceType) : null);

	let durationText = $derived(
		playlist.total_duration ? formatTotalDurationSec(playlist.total_duration) : ''
	);
	let countText = $derived(`${playlist.track_count} track${playlist.track_count === 1 ? '' : 's'}`);

	let editingName = $state(false);
	let nameInput = $state('');
	let savingName = $state(false);
	let nameInputEl = $state<HTMLInputElement | null>(null);

	let coverInput = $state<HTMLInputElement | null>(null);
	let uploading = $state(false);
	let coverPreview = $state<string | null>(null);
	let discoverModalOpen = $state(false);

	const renameMutation = createUpdatePlaylistV3Mutation();
	const uploadCoverMutation = createUploadPlaylistCoverV3Mutation();
	const removeCoverMutation = createDeletePlaylistCoverV3Mutation();

	$effect(() => {
		if (editingName && nameInputEl) {
			nameInputEl.focus();
			nameInputEl.select();
		}
	});

	function buildUpdatedPlaylist(changes: Partial<PlaylistDetail>): PlaylistDetail {
		return {
			...playlist,
			...changes
		};
	}

	function startEditName() {
		nameInput = playlist.name;
		editingName = true;
	}

	async function saveName() {
		if (savingName) return;
		const trimmed = nameInput.trim();
		if (!trimmed || trimmed === playlist.name) {
			editingName = false;
			return;
		}
		savingName = true;
		try {
			const renamed = await renameMutation.mutateAsync({ id: playlist.id, name: trimmed });
			const updated = toPageDetail(renamed);
			editingName = false;
			onplaylistupdate(
				buildUpdatedPlaylist({
					name: updated.name,
					updated_at: updated.updated_at
				})
			);
			toastStore.show({ message: 'Playlist renamed.', type: 'success' });
		} catch {
			toastStore.show({ message: "Couldn't rename that playlist.", type: 'error' });
		} finally {
			savingName = false;
		}
	}

	function cancelEditName() {
		editingName = false;
		nameInput = '';
	}

	function handleNameKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter') void saveName();
		if (e.key === 'Escape') cancelEditName();
	}

	function triggerCoverUpload() {
		coverInput?.click();
	}

	async function handleCoverSelect(e: Event) {
		const input = e.target as HTMLInputElement;
		const file = input.files?.[0];
		if (!file) return;

		if (!file.type.match(/^image\/(jpeg|png|webp)$/)) {
			toastStore.show({ message: 'Choose a JPEG, PNG, or WebP image.', type: 'error' });
			input.value = '';
			return;
		}
		if (file.size > 2 * 1024 * 1024) {
			toastStore.show({ message: 'The image must be smaller than 2 MB.', type: 'error' });
			input.value = '';
			return;
		}

		coverPreview = URL.createObjectURL(file);
		uploading = true;
		try {
			const result = await uploadCoverMutation.mutateAsync({ id: playlist.id, file });
			onplaylistupdate(
				buildUpdatedPlaylist({
					custom_cover_url: result.cover_url + '?t=' + Date.now()
				})
			);
			toastStore.show({ message: 'Cover updated.', type: 'success' });
		} catch {
			toastStore.show({ message: "Couldn't upload that cover image.", type: 'error' });
		} finally {
			if (coverPreview) {
				URL.revokeObjectURL(coverPreview);
				coverPreview = null;
			}
			uploading = false;
			input.value = '';
		}
	}

	async function removeCover() {
		try {
			await removeCoverMutation.mutateAsync(playlist.id);
			onplaylistupdate(buildUpdatedPlaylist({ custom_cover_url: null }));
			toastStore.show({ message: 'Cover removed.', type: 'success' });
		} catch {
			toastStore.show({ message: "Couldn't remove that cover image.", type: 'error' });
		}
	}

	const menuItems = $derived.by(() => {
		const items: MenuItem[] = [];
		if (canEdit) {
			items.push({ label: 'Edit name', icon: PencilLine, onclick: startEditName });
			items.push({
				label: playlist.is_public ? 'Make private' : 'Make public',
				icon: playlist.is_public ? Lock : Globe,
				disabled: sharePending,
				onclick: () => onshare?.(!playlist.is_public)
			});
		}
		if (canDelete) {
			items.push({
				label: 'Delete playlist',
				icon: Trash2,
				className: 'text-error',
				onclick: ondeleteclick
			});
		}
		return items;
	});

	// A long name steps down so it wraps to a few lines, never clips.
	const titleSize = $derived(
		playlist.name.length > 40 ? 'long' : playlist.name.length > 16 ? 'mid' : 'short'
	);

	let coverDisplayUrl = $derived(coverPreview ?? playlist.custom_cover_url ?? null);

	export function cleanupPreview() {
		if (coverPreview) {
			URL.revokeObjectURL(coverPreview);
			coverPreview = null;
		}
	}
</script>

<section class="pl-hero" aria-labelledby="playlist-title">
	<div class="dn-decor pl-hero__wash" aria-hidden="true"></div>

	<div class="pl-hero__inner">
		<div class="pl-hero__top">
			<IconButton
				variant="outline"
				icon={ArrowLeft}
				label="Back to Playlists"
				href={withBasePath('/playlists')}
			/>
			{#if menuItems.length > 0}
				<ContextMenu items={menuItems} position="end" trigger="button" label="Playlist actions" />
			{/if}
		</div>

		<div class="pl-hero__grid">
			<VinylSleeve class="pl-hero__sleeve">
				<div class="group relative">
					<PlaylistMosaic
						coverUrls={playlist.cover_urls}
						customCoverUrl={coverDisplayUrl}
						size="w-full aspect-square"
						rounded="rounded-none"
					/>
					{#if uploading}
						<div class="absolute inset-0 grid place-items-center bg-base-100/60">
							<span class="loading loading-spinner loading-md"></span>
						</div>
					{/if}
					{#if canEdit}
						<div
							class="absolute right-2 bottom-2 flex gap-1.5 transition-opacity [@media(hover:hover)]:opacity-0 [@media(hover:hover)]:group-focus-within:opacity-100 [@media(hover:hover)]:group-hover:opacity-100"
						>
							<IconButton
								variant="neutral"
								size="sm"
								icon={Pencil}
								label="Upload cover image"
								onclick={triggerCoverUpload}
								disabled={uploading}
							/>
							{#if playlist.custom_cover_url}
								<IconButton
									variant="neutral"
									size="sm"
									icon={X}
									label="Remove custom cover"
									onclick={() => void removeCover()}
								/>
							{/if}
						</div>
					{/if}
				</div>
			</VinylSleeve>
			<input
				type="file"
				accept="image/jpeg,image/png,image/webp"
				class="hidden"
				aria-label="Cover image"
				bind:this={coverInput}
				onchange={handleCoverSelect}
			/>

			<div class="pl-hero__main">
				{#if editingName && canEdit}
					<div class="pl-hero__rename">
						<Input
							label="Playlist name"
							bind:ref={nameInputEl}
							bind:value={nameInput}
							onkeydown={handleNameKeydown}
							class="min-w-0 flex-1"
							placeholder="Playlist name"
							maxlength={100}
						/>
						<IconButton
							variant="primary"
							shape="square"
							icon={Check}
							label="Save name"
							loading={savingName}
							onclick={() => void saveName()}
						/>
						<IconButton
							variant="outline"
							shape="square"
							icon={X}
							label="Cancel"
							onclick={cancelEditName}
						/>
					</div>
				{:else}
					<h1 id="playlist-title" class="pl-hero__title" data-size={titleSize}>
						{playlist.name}
					</h1>
				{/if}

				<div class="pl-hero__facts">
					<p class="pl-hero__fact">
						{#if playlist.is_public}
							<Globe class="size-4" aria-hidden="true" /> Public playlist
						{:else}
							<Lock class="size-4" aria-hidden="true" /> Private playlist
						{/if}
					</p>
					<p class="pl-hero__fact">
						<span>{countText}</span>
						{#if durationText}
							<span aria-hidden="true">·</span>
							<span>{durationText}</span>
						{/if}
						<span aria-hidden="true">·</span>
						{#if sourceType && sourceLabel}
							<span class="inline-flex items-center gap-2" style:--pl-source={sourceColor}>
								<span class="pl-hero__source" aria-hidden="true">
									{#if sourceType === 'jellyfin'}
										<JellyfinIcon class="size-3.5" />
									{:else if sourceType === 'navidrome'}
										<NavidromeIcon class="size-3.5" />
									{:else if sourceType === 'plex'}
										<PlexIcon class="size-3.5" />
									{/if}
								</span>
								Imported from {sourceLabel}
							</span>
						{:else}
							<span>Created {formatRelativeTime(new Date(playlist.created_at))}</span>
						{/if}
					</p>
					{#if !playlist.is_owner && playlist.owner_name}
						<p class="pl-hero__fact">
							<span class="pl-hero__initials" aria-hidden="true">{ownerInitials}</span>
							Shared by {playlist.owner_name}
						</p>
					{/if}
				</div>

				<div class="pl-hero__actions">
					<Button
						variant="primary"
						size="display"
						icon={Play}
						class="[&_svg]:fill-current"
						onclick={onplayall}
						disabled={playlist.track_count === 0}
					>
						Play All
					</Button>
					<Button
						variant="outline"
						size="display"
						icon={Shuffle}
						class="pl-hero__shuffle"
						onclick={onshuffleall}
						disabled={playlist.track_count < 2}
					>
						<span class="pl-hero__shuffle-label">Shuffle</span>
					</Button>
					<Button
						variant="outline"
						size="display"
						icon={Sparkles}
						class="pl-hero__suggest"
						onclick={() => (discoverModalOpen = true)}
						disabled={playlist.track_count === 0}
					>
						Suggest Albums
					</Button>
				</div>
			</div>
		</div>
	</div>
</section>

<PlaylistDiscoveryModal
	bind:open={discoverModalOpen}
	playlistId={playlist.id}
	playlistName={playlist.name}
	playlistCoverUrls={playlist.cover_urls}
	playlistCustomCoverUrl={playlist.custom_cover_url}
	playlistMeta={[countText, durationText, sourceLabel ? `from ${sourceLabel}` : '']
		.filter(Boolean)
		.join(' · ')}
/>

<style>
	.pl-hero {
		position: relative;
		isolation: isolate;
		overflow: clip;
	}

	/* Light only: a faint green cloud behind the words, as on an album. */
	.pl-hero__wash {
		display: none;
		position: absolute;
		inset: 0;
		z-index: -1;
		pointer-events: none;
	}
	:global([data-theme='light']) .pl-hero__wash {
		display: block;
		background:
			radial-gradient(
				ellipse 50% 75% at 45% 45%,
				color-mix(in oklab, var(--dn-action) 30%, transparent),
				transparent 72%
			),
			radial-gradient(
				ellipse 35% 80% at 100% 0%,
				color-mix(in oklab, var(--dn-action) 26%, transparent),
				transparent 75%
			);
	}

	.pl-hero__inner {
		width: 100%;
		max-width: 80rem;
		margin: 0 auto;
		padding: 1rem 1rem 1.5rem;
	}

	.pl-hero__top {
		display: flex;
		align-items: center;
		justify-content: space-between;
		margin-bottom: 0.75rem;
	}

	.pl-hero__grid {
		display: grid;
		gap: 1.5rem;
		align-items: center;
	}
	.pl-hero__grid > :global(.pl-hero__sleeve) {
		justify-self: center;
		width: min(100%, 19rem);
	}

	.pl-hero__main {
		display: flex;
		min-width: 0;
		flex-direction: column;
		align-items: flex-start;
	}

	.pl-hero__title {
		max-width: 100%;
		font-size: clamp(2.75rem, 1.1rem + 4.3vw, 5.25rem);
		line-height: 0.92;
		overflow-wrap: break-word;
		hyphens: auto;
		text-wrap: balance;
	}
	.pl-hero__title[data-size='mid'] {
		font-size: clamp(2.25rem, 0.9rem + 3.3vw, 4rem);
	}
	.pl-hero__title[data-size='long'] {
		font-size: clamp(1.875rem, 0.85rem + 2.3vw, 2.875rem);
		line-height: 0.98;
	}

	.pl-hero__rename {
		display: flex;
		width: 100%;
		max-width: 36rem;
		align-items: center;
		gap: 0.5rem;
	}

	.pl-hero__facts {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		margin-top: 1rem;
	}
	.pl-hero__fact {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.25rem 0.625rem;
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 600;
		letter-spacing: 0.14em;
		text-transform: uppercase;
	}
	.pl-hero__source {
		display: grid;
		place-items: center;
		width: 1.375rem;
		height: 1.375rem;
		border-radius: 0.3125rem;
		background-color: color-mix(in oklab, var(--pl-source) 82%, #000);
		color: #fff;
	}
	.pl-hero__initials {
		display: grid;
		place-items: center;
		width: 1.5rem;
		height: 1.5rem;
		border-radius: 9999px;
		background-color: var(--dn-action);
		color: var(--dn-action-content);
		font-size: 0.625rem;
		letter-spacing: 0.02em;
	}

	.pl-hero__actions {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.75rem;
		margin-top: 1.5rem;
	}

	@media (min-width: 480px) {
		.pl-hero__grid > :global(.pl-hero__sleeve) {
			width: min(100%, 24rem);
		}
	}

	@media (min-width: 768px) {
		.pl-hero__inner {
			padding: 1.25rem 1.5rem 2rem;
		}
		.pl-hero__grid {
			grid-template-columns: 17rem minmax(0, 1fr);
			gap: 2.25rem;
		}
		.pl-hero__grid > :global(.pl-hero__sleeve) {
			width: 100%;
		}
	}

	@media (min-width: 1024px) {
		.pl-hero__inner {
			padding: 1.5rem 2rem 2.25rem;
		}
		.pl-hero__grid {
			grid-template-columns: 21rem minmax(0, 1fr);
			gap: 3rem;
		}
	}

	@media (min-width: 1280px) {
		.pl-hero__grid {
			grid-template-columns: 24rem minmax(0, 1fr);
		}
	}

	/* Phone: Play All with a square Shuffle beside it, Suggest Albums under them */
	@media (max-width: 767px) {
		.pl-hero__main {
			align-items: stretch;
		}
		.pl-hero__actions {
			display: grid;
			grid-template-columns: minmax(0, 1fr) auto;
			gap: 0.625rem;
		}
		.pl-hero__actions :global(.pl-hero__shuffle) {
			width: var(--size);
			padding-inline: 0;
		}
		.pl-hero__shuffle-label {
			position: absolute;
			width: 1px;
			height: 1px;
			overflow: hidden;
			clip-path: inset(50%);
			white-space: nowrap;
		}
		.pl-hero__actions :global(.pl-hero__suggest) {
			grid-column: 1 / -1;
		}
	}
</style>
