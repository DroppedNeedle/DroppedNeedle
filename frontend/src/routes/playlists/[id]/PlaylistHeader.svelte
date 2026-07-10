<script lang="ts">
	import {
		updatePlaylist,
		uploadPlaylistCover,
		deletePlaylistCover,
		type PlaylistDetail
	} from '$lib/api/playlists';
	import PlaylistDiscoveryModal from '$lib/components/PlaylistDiscoveryModal.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { formatTotalDurationSec, formatRelativeTime } from '$lib/utils/formatting';
	import PlaylistMosaic from '$lib/components/PlaylistMosaic.svelte';
	import ContextMenu from '$lib/components/ContextMenu.svelte';
	import type { MenuItem } from '$lib/components/ContextMenu.svelte';
	import { Play, Shuffle, Pencil, Check, X, Tv, Sparkles, Globe, Lock } from 'lucide-svelte';
	import NavidromeIcon from '$lib/components/NavidromeIcon.svelte';
	import PlexIcon from '$lib/components/PlexIcon.svelte';
	import { getSourceColor, getSourceLabel } from '$lib/utils/sources';

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

	import { Trash2 } from 'lucide-svelte';

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

	let editingName = $state(false);
	let nameInput = $state('');
	let savingName = $state(false);
	let nameInputEl = $state<HTMLInputElement | null>(null);
	let saveBtnEl = $state<HTMLButtonElement | null>(null);

	let coverInput = $state<HTMLInputElement | null>(null);
	let uploading = $state(false);
	let coverPreview = $state<string | null>(null);
	let discoverModalOpen = $state(false);

	$effect(() => {
		if (editingName && nameInputEl) {
			nameInputEl.focus();
			nameInputEl.select();
		}
	});

	$effect(() => {
		if (saveBtnEl) {
			const btn = saveBtnEl;
			const handler = () => {
				void saveName();
			};
			btn.addEventListener('click', handler);
			return () => btn.removeEventListener('click', handler);
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
			const updated = await updatePlaylist(playlist.id, { name: trimmed });
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
			const result = await uploadPlaylistCover(playlist.id, file);
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
			await deletePlaylistCover(playlist.id);
			onplaylistupdate(buildUpdatedPlaylist({ custom_cover_url: null }));
			toastStore.show({ message: 'Cover removed.', type: 'success' });
		} catch {
			toastStore.show({ message: "Couldn't remove that cover image.", type: 'error' });
		}
	}

	function getHeaderMenuItems(): MenuItem[] {
		return [
			{
				label: 'Delete playlist',
				icon: Trash2,
				className: 'text-error',
				onclick: ondeleteclick
			}
		];
	}

	let coverDisplayUrl = $derived(coverPreview ?? playlist.custom_cover_url ?? null);

	export function cleanupPreview() {
		if (coverPreview) {
			URL.revokeObjectURL(coverPreview);
			coverPreview = null;
		}
	}
</script>

<div class="flex flex-col gap-6 lg:flex-row lg:items-end lg:gap-10">
	<div class="group relative w-full max-w-72 shrink-0 sm:max-w-80 lg:w-60 xl:w-72">
		<PlaylistMosaic
			coverUrls={playlist.cover_urls}
			customCoverUrl={coverDisplayUrl}
			size="w-full aspect-square"
			rounded="rounded-2xl"
		/>
		<div
			class="pointer-events-none absolute inset-0 rounded-2xl border border-base-content/10 shadow-2xl"
		></div>
		{#if uploading}
			<div class="absolute inset-0 flex items-center justify-center rounded-2xl bg-base-100/60">
				<span class="loading loading-spinner loading-md"></span>
			</div>
		{/if}
		{#if canEdit}
			<div
				class="absolute bottom-2 right-2 flex gap-1 opacity-0 transition-opacity group-focus-within:opacity-100 group-hover:opacity-100"
			>
				<button
					type="button"
					class="btn btn-circle btn-xs border-0 bg-base-100/80 hover:bg-base-100"
					onclick={triggerCoverUpload}
					aria-label="Upload cover image"
					disabled={uploading}
				>
					<Pencil class="h-3 w-3" />
				</button>
				{#if playlist.custom_cover_url}
					<button
						type="button"
						class="btn btn-circle btn-xs border-0 bg-base-100/80 hover:bg-error/80"
						onclick={() => void removeCover()}
						aria-label="Remove custom cover"
					>
						<X class="h-3 w-3" />
					</button>
				{/if}
			</div>
		{/if}
		<input
			type="file"
			accept="image/jpeg,image/png,image/webp"
			class="hidden"
			bind:this={coverInput}
			onchange={handleCoverSelect}
		/>
	</div>

	<div class="flex min-w-0 flex-1 flex-col gap-2 lg:justify-end">
		<span
			class="w-fit rounded-full border border-base-content/15 bg-base-100/45 px-3 py-1 font-mono text-[0.62rem] font-bold uppercase tracking-[0.22em] text-base-content/70 backdrop-blur-sm"
		>
			Playlist
		</span>

		{#if editingName && canEdit}
			<div class="flex items-center gap-2">
				<input
					type="text"
					bind:this={nameInputEl}
					bind:value={nameInput}
					onkeydown={handleNameKeydown}
					class="input input-sm w-full font-display text-3xl font-bold sm:text-4xl"
					placeholder="Playlist name"
					maxlength={100}
				/>
				<button
					bind:this={saveBtnEl}
					class="btn btn-ghost btn-sm btn-circle"
					aria-label="Save name"
				>
					<Check class="h-4 w-4 text-success" />
				</button>
				<button
					onclick={cancelEditName}
					class="btn btn-ghost btn-sm btn-circle"
					aria-label="Cancel"
				>
					<X class="h-4 w-4 text-error" />
				</button>
			</div>
		{:else if canEdit}
			<button
				type="button"
				onclick={startEditName}
				class="group/name flex items-center gap-2 text-left"
				aria-label="Edit playlist name"
			>
				<h1
					class="hero-title truncate font-display text-3xl font-bold leading-tight tracking-tight sm:text-4xl lg:text-5xl xl:text-6xl"
				>
					{playlist.name}
				</h1>
				<Pencil
					class="h-4 w-4 shrink-0 text-base-content/30 opacity-0 transition-opacity group-hover/name:opacity-100"
				/>
			</button>
		{:else}
			<h1
				class="hero-title truncate font-display text-3xl font-bold leading-tight tracking-tight sm:text-4xl lg:text-5xl xl:text-6xl"
			>
				{playlist.name}
			</h1>
		{/if}

		<div class="flex flex-wrap items-center gap-2 text-sm text-base-content/60">
			<span>{playlist.track_count} track{playlist.track_count === 1 ? '' : 's'}</span>
			{#if playlist.total_duration}
				<span class="opacity-50">·</span>
				<span>{formatTotalDurationSec(playlist.total_duration)}</span>
			{/if}
			<span class="opacity-50">·</span>
			<span>Created {formatRelativeTime(new Date(playlist.created_at))}</span>
			{#if sourceType && sourceColor && sourceLabel}
				<span class="opacity-50">·</span>
				<span
					class="inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-xs font-medium"
					style="background: color-mix(in srgb, {sourceColor} 15%, transparent); color: {sourceColor};"
				>
					{#if sourceType === 'jellyfin'}
						<Tv class="h-3 w-3" />
					{:else if sourceType === 'navidrome'}
						<NavidromeIcon class="h-3 w-3" />
					{:else if sourceType === 'plex'}
						<PlexIcon class="h-3 w-3" />
					{/if}
					Imported from {sourceLabel}
				</span>
			{/if}
		</div>

		<div class="flex flex-wrap items-center gap-3 pt-4">
			<button
				type="button"
				class="btn btn-primary gap-2 rounded-full"
				onclick={onplayall}
				disabled={playlist.track_count === 0}
			>
				<Play class="h-4 w-4" />
				Play All
			</button>
			<button
				type="button"
				class="btn btn-ghost gap-2 rounded-full bg-base-100/40"
				onclick={onshuffleall}
				disabled={playlist.track_count < 2}
			>
				<Shuffle class="h-4 w-4" />
				Shuffle
			</button>
			<button
				type="button"
				class="btn btn-ghost gap-2 rounded-full bg-base-100/40"
				onclick={() => (discoverModalOpen = true)}
				disabled={playlist.track_count === 0}
			>
				<Sparkles class="h-4 w-4" />
				Discover
			</button>

			{#if canEdit}
				<label
					class="ml-1 flex cursor-pointer items-center gap-2 rounded-full border border-base-content/10 bg-base-100/40 px-3 py-1.5 backdrop-blur-sm"
					title="Toggle who can see this playlist"
				>
					{#if playlist.is_public}
						<Globe class="h-4 w-4 text-success" />
					{:else}
						<Lock class="h-4 w-4 text-base-content/50" />
					{/if}
					<span class="text-sm text-base-content/70"
						>{playlist.is_public ? 'Public' : 'Private'}</span
					>
					<input
						type="checkbox"
						class="toggle toggle-sm toggle-success"
						checked={playlist.is_public}
						disabled={sharePending}
						onchange={(e) => onshare?.(e.currentTarget.checked)}
						aria-label={playlist.is_public ? 'Make playlist private' : 'Make playlist public'}
					/>
				</label>
			{:else if !playlist.is_owner && playlist.owner_name}
				<span class="ml-1 inline-flex items-center gap-1.5 text-sm text-base-content/60">
					<span class="avatar avatar-placeholder">
						<span class="h-5 w-5 rounded-full bg-accent text-accent-content">
							<span class="text-[9px] font-semibold leading-none">{ownerInitials}</span>
						</span>
					</span>
					Shared by {playlist.owner_name}
				</span>
			{/if}

			{#if canDelete}
				<ContextMenu items={getHeaderMenuItems()} position="end" size="sm" />
			{/if}
		</div>
	</div>
</div>

<PlaylistDiscoveryModal
	bind:open={discoverModalOpen}
	playlistId={playlist.id}
	playlistName={playlist.name}
/>
