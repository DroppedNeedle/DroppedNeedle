<script lang="ts">
	import { ChevronDown, FolderTree } from 'lucide-svelte';
	import type { LibraryAlbumDetail, NativeTrackListItem } from '$lib/types';
	import type { MembershipPreviewResponse } from '$lib/queries/library/LibraryOperationsTypes';
	import {
		getLibraryAlbumDetailQuery,
		getLibraryAlbumsQuery
	} from '$lib/queries/library/LibraryQueries.svelte';
	import {
		applyAlbumMembership,
		previewAlbumMembership,
		type MembershipPreviewInput
	} from '$lib/queries/library/LibraryCatalogMutations.svelte';

	type Action = 'split' | 'merge' | 'move' | 'reset';
	interface Props {
		album: LibraryAlbumDetail;
		tracks: NativeTrackListItem[];
	}
	let { album, tracks }: Props = $props();
	let dialog: HTMLDialogElement;
	let dialogHeading: HTMLHeadingElement;
	let opener: HTMLButtonElement | null = null;
	let action = $state<Action>('split');
	let selectedTrackIds = $state<string[]>([]);
	let targetSearch = $state('');
	let targetAlbumId = $state<string | null>(null);
	let identityChoice = $state<'detach' | 'retain_manual'>('detach');
	let confirmed = $state(false);
	let stalePreview = $state(false);
	let previewResult = $state<MembershipPreviewResponse | null>(null);
	const targetAlbums = getLibraryAlbumsQuery(() => ({
		limit: 48,
		offset: 0,
		sort: 'name',
		order: 'asc',
		...(targetSearch ? { q: targetSearch } : {})
	}));
	const targetAlbum = getLibraryAlbumDetailQuery(() => targetAlbumId ?? '');
	const splitPreview = previewAlbumMembership('split');
	const mergePreview = previewAlbumMembership('merge');
	const movePreview = previewAlbumMembership('move');
	const resetPreview = previewAlbumMembership('reset');
	const splitApply = applyAlbumMembership('split');
	const mergeApply = applyAlbumMembership('merge');
	const moveApply = applyAlbumMembership('move');
	const resetApply = applyAlbumMembership('reset');

	const previewMutation = $derived(
		action === 'split'
			? splitPreview
			: action === 'merge'
				? mergePreview
				: action === 'move'
					? movePreview
					: resetPreview
	);
	const applyMutation = $derived(
		action === 'split'
			? splitApply
			: action === 'merge'
				? mergeApply
				: action === 'move'
					? moveApply
					: resetApply
	);

	function open(next: Action, event: MouseEvent & { currentTarget: HTMLButtonElement }): void {
		opener = event.currentTarget;
		action = next;
		selectedTrackIds = next === 'reset' || next === 'merge' ? tracks.map((track) => track.id) : [];
		targetSearch = '';
		targetAlbumId = null;
		confirmed = false;
		stalePreview = false;
		previewMutation.reset();
		previewResult = null;
		dialog.showModal();
		dialogHeading.focus();
	}

	function toggleTrack(trackId: string, selected: boolean): void {
		selectedTrackIds = selected
			? [...new Set([...selectedTrackIds, trackId])]
			: selectedTrackIds.filter((id) => id !== trackId);
		previewMutation.reset();
		previewResult = null;
	}

	function request(): MembershipPreviewInput {
		const revisions: Record<string, number> = { [album.id]: album.row_revision };
		if (targetAlbum.data) revisions[targetAlbum.data.id] = targetAlbum.data.row_revision;
		return {
			track_ids: selectedTrackIds,
			expected_album_revisions: revisions,
			target_album_id: targetAlbumId
		};
	}

	async function preview(): Promise<void> {
		confirmed = false;
		stalePreview = false;
		try {
			previewResult = await previewMutation.mutateAsync({
				albumId: album.id,
				request: request()
			});
		} catch {
			previewResult = null;
		}
	}

	async function apply(): Promise<void> {
		if (!previewResult || !confirmed) return;
		try {
			await applyMutation.mutateAsync({
				albumId: album.id,
				request: request(),
				previewToken: previewResult.preview_token,
				identityChoice
			});
			dialog.close();
		} catch {
			confirmed = false;
			stalePreview = true;
			previewMutation.reset();
			previewResult = null;
		}
	}

	const needsTarget = $derived(action === 'merge' || action === 'move');
	const canPreview = $derived(
		selectedTrackIds.length > 0 && (!needsTarget || (targetAlbumId !== null && !!targetAlbum.data))
	);
	const title = $derived(
		action === 'split'
			? 'Split album'
			: action === 'merge'
				? 'Merge with another local album'
				: action === 'move'
					? 'Move tracks'
					: 'Reset manual grouping'
	);
</script>

<details class="dropdown dropdown-end">
	<summary class="btn btn-ghost btn-sm gap-2">
		<FolderTree class="h-4 w-4" /> Album organization <ChevronDown class="h-3.5 w-3.5" />
	</summary>
	<ul class="menu dropdown-content z-30 mt-2 w-64 rounded-box bg-base-100 p-2 shadow-xl">
		<li><button onclick={(event) => open('split', event)}>Split album...</button></li>
		<li>
			<button onclick={(event) => open('merge', event)}>Merge with another local album...</button>
		</li>
		<li><button onclick={(event) => open('move', event)}>Move tracks...</button></li>
		<li><button onclick={(event) => open('reset', event)}>Reset manual grouping...</button></li>
	</ul>
</details>

<dialog
	bind:this={dialog}
	class="modal"
	aria-labelledby="album-organization-title"
	onclose={() => opener?.focus()}
>
	<div class="modal-box max-w-4xl">
		<h2
			bind:this={dialogHeading}
			id="album-organization-title"
			tabindex="-1"
			class="text-xl font-bold"
		>
			{title}
		</h2>
		<p class="mt-1 text-sm text-base-content/60">
			This changes local album membership only. It will not move files or rewrite tags.
		</p>
		{#if stalePreview}
			<div class="alert alert-warning mt-4 text-sm">
				The local grouping changed after this preview. Review the current tracks and preview the
				change again.
			</div>
		{/if}
		{#if previewMutation.isError}
			<div class="alert alert-error mt-4 text-sm">
				Could not preview this grouping change. Nothing has been changed.
			</div>
		{/if}

		{#if needsTarget}
			<section class="mt-5" aria-labelledby="target-album-title">
				<h3 id="target-album-title" class="font-semibold">Choose the other local album</h3>
				<input
					class="input input-bordered mt-2 w-full"
					placeholder="Search local albums"
					bind:value={targetSearch}
				/>
				{#if targetSearch.trim().length >= 2}
					<div class="mt-2 max-h-44 overflow-auto rounded-box border border-base-content/10 p-2">
						{#each (targetAlbums.data?.items ?? []).filter((item) => item.id !== album.id) as item (item.id)}
							<label
								class="flex cursor-pointer items-center gap-3 rounded-lg p-2 hover:bg-base-200"
							>
								<input
									type="radio"
									name="target-local-album"
									class="radio radio-sm"
									checked={targetAlbumId === item.id}
									onchange={() => {
										targetAlbumId = item.id;
										previewMutation.reset();
										previewResult = null;
									}}
								/>
								<span
									><strong>{item.title}</strong><span class="block text-xs text-base-content/55"
										>{item.artist_name}</span
									></span
								>
							</label>
						{/each}
					</div>
				{/if}
			</section>
		{/if}

		{#if action !== 'reset'}
			<fieldset class="mt-5">
				<legend class="font-semibold">Tracks included</legend>
				<div
					class="mt-2 grid max-h-64 gap-1 overflow-auto rounded-box border border-base-content/10 p-2 md:grid-cols-2"
				>
					{#each tracks as track (track.id)}
						<label class="flex cursor-pointer items-center gap-2 rounded-lg p-2 hover:bg-base-200">
							<input
								type="checkbox"
								class="checkbox checkbox-sm"
								checked={selectedTrackIds.includes(track.id)}
								onchange={(event) => toggleTrack(track.id, event.currentTarget.checked)}
							/>
							<span class="truncate">{track.disc_number}.{track.track_number} {track.title}</span>
						</label>
					{/each}
				</div>
			</fieldset>
		{/if}

		{#if previewResult}
			{@const result = previewResult}
			<section
				class="mt-5 rounded-box bg-base-200/60 p-4"
				aria-labelledby="organization-preview-title"
			>
				<h3 id="organization-preview-title" class="font-semibold">Preview</h3>
				<p class="mt-1 text-sm">
					{result.track_ids.length} tracks · {result.source_album_ids.length} source albums · {result
						.aliases.length}
					aliases retained
				</p>
				{#if Object.keys(result.reference_counts).length}
					<dl class="mt-3 grid gap-2 text-sm sm:grid-cols-3">
						{#each Object.entries(result.reference_counts) as [kind, count] (kind)}
							<div>
								<dt class="text-base-content/55">{kind.replaceAll('_', ' ')}</dt>
								<dd class="font-semibold">{count}</dd>
							</div>
						{/each}
					</dl>
				{/if}
				{#if result.identity_conflicts.length}
					<div class="alert alert-warning mt-3 text-sm">
						<div class="w-full">
							<p>
								External identities conflict. Choose what the resulting local album should retain.
							</p>
							<label class="mt-2 flex items-center gap-2"
								><input
									type="radio"
									class="radio radio-sm"
									bind:group={identityChoice}
									value="detach"
								/> Detach conflicting identities</label
							>
							<label class="mt-2 flex items-center gap-2"
								><input
									type="radio"
									class="radio radio-sm"
									bind:group={identityChoice}
									value="retain_manual"
								/> Retain the target identity for manual review</label
							>
						</div>
					</div>
				{/if}
				<label class="mt-4 flex items-start gap-2 text-sm">
					<input type="checkbox" class="checkbox checkbox-sm mt-0.5" bind:checked={confirmed} />
					<span>I understand this changes local grouping and preserves files and tags.</span>
				</label>
			</section>
		{/if}

		<div class="modal-action">
			<form method="dialog"><button class="btn btn-ghost">Cancel</button></form>
			{#if previewResult}
				<button
					class="btn btn-primary"
					disabled={!confirmed || applyMutation.isPending}
					onclick={() => void apply()}
				>
					Apply {title.toLowerCase()}
				</button>
			{:else}
				<button
					class="btn btn-primary"
					disabled={!canPreview || previewMutation.isPending}
					onclick={() => void preview()}
				>
					Preview changes
				</button>
			{/if}
		</div>
	</div>
	<form method="dialog" class="modal-backdrop"><button>close</button></form>
</dialog>
