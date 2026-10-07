<script lang="ts">
	import { Check, Disc3, Download, Search, Tags, Undo2, X } from 'lucide-svelte';
	import { ApiError } from '$lib/api/client';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { getLibraryAlbumTracksQuery } from '$lib/queries/library/LibraryQueries.svelte';
	import {
		acquireEdition,
		chooseAlbumEdition,
		getAlbumEditionStatusQuery,
		getAlbumEditionsQuery,
		getEditionTracksQuery,
		handBackAlbumEdition,
		retagAfterChoice,
		undoAlbumEdition,
		type EditionChoice
	} from '$lib/queries/albums/EditionQueries.svelte';
	import type { AlbumEditionItem } from '$lib/types';

	interface Copy {
		id: string;
		title: string;
	}

	interface Props {
		/** The release group whose editions are listed (empty: paste an ID only). */
		groupMbid: string;
		/** Library copies of the album; the choice applies to one of them. */
		copies: Copy[];
		onchanged?: () => void;
	}

	let { groupMbid, copies, onchanged }: Props = $props();

	let dialog = $state<HTMLDialogElement | null>(null);
	let copyId = $state('');
	let filter = $state('');
	let pasted = $state('');
	let selected = $state<string | null>(null);
	let result = $state<EditionChoice | null>(null);
	let retagged = $state(false);
	let acquired = $state(false);

	const userId = () => authStore.user?.id;
	const statusQuery = getAlbumEditionStatusQuery(userId, () => copyId);
	const editionsQuery = getAlbumEditionsQuery(
		userId,
		() => groupMbid,
		() => Boolean(groupMbid)
	);
	const filesQuery = getLibraryAlbumTracksQuery(() => copyId);
	const tracksQuery = getEditionTracksQuery(
		() => groupMbid || copyId,
		() => selected
	);
	const choose = chooseAlbumEdition();
	const handBack = handBackAlbumEdition();
	const undo = undoAlbumEdition();
	const retag = retagAfterChoice();
	const acquire = acquireEdition();

	const status = $derived(statusQuery.data ?? null);
	const editions = $derived(editionsQuery.data?.items ?? []);
	const files = $derived(filesQuery.data?.items ?? []);
	const editionTracks = $derived(tracksQuery.data?.tracks ?? []);
	const visible = $derived(
		editions.filter((edition) => {
			const needle = filter.trim().toLowerCase();
			if (!needle) return true;
			return [
				edition.title,
				edition.disambiguation,
				edition.date,
				edition.country,
				edition.barcode,
				edition.catalog_number,
				...(edition.media_formats ?? [])
			]
				.filter(Boolean)
				.some((text) => String(text).toLowerCase().includes(needle));
		})
	);
	const pastedMbid = $derived(
		pasted.match(/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i)?.[0] ?? null
	);

	type Row = { key: string; file: string | null; edition: string | null; same: boolean };
	// Files and edition tracks side by side, by disc and position.
	const rows = $derived.by((): Row[] => {
		const keyOf = (disc: number, position: number) => `${Math.max(1, disc)}-${position}`;
		const byKey: Record<string, Row> = {};
		for (const file of files) {
			const key = keyOf(file.disc_number, file.track_number);
			byKey[key] = { key, file: file.title, edition: null, same: false };
		}
		for (const track of editionTracks) {
			const key = keyOf(track.disc_number, track.position);
			const row = byKey[key] ?? { key, file: null, edition: null, same: false };
			row.edition = track.title;
			row.same = fold(row.file) !== '' && fold(row.file) === fold(track.title);
			byKey[key] = row;
		}
		return Object.values(byKey).sort((a, b) => {
			const [ad, ap] = a.key.split('-').map(Number);
			const [bd, bp] = b.key.split('-').map(Number);
			return ad - bd || ap - bp;
		});
	});
	const fits = $derived(rows.filter((row) => row.same).length);

	function fold(text: string | null): string {
		return (text ?? '').toLowerCase().replace(/[^\p{L}\p{N}]+/gu, '');
	}

	function label(edition: AlbumEditionItem): string {
		return (
			[
				edition.disambiguation,
				edition.date?.slice(0, 4),
				edition.country,
				(edition.media_formats ?? []).join(' + ')
			]
				.filter(Boolean)
				.join(' · ') || edition.release_mbid.slice(0, 8)
		);
	}

	function failure(error: unknown, fallback: string): string {
		if (error instanceof ApiError) {
			const action = (error.details as { action?: unknown } | null)?.action;
			return typeof action === 'string' ? `${error.message} ${action}` : error.message;
		}
		return error instanceof Error ? error.message : fallback;
	}

	export function open(): void {
		copyId = copies[0]?.id ?? '';
		selected = status?.release_mbid ?? null;
		result = null;
		retagged = false;
		acquired = false;
		filter = '';
		pasted = '';
		dialog?.showModal();
	}

	$effect(() => {
		if (selected === null && status?.release_mbid) selected = status.release_mbid;
	});

	async function useEdition(release: string): Promise<void> {
		try {
			result = await choose.mutateAsync({
				userId: userId(),
				localId: copyId,
				rgMbid: groupMbid,
				releaseMbid: release
			});
			onchanged?.();
		} catch (error) {
			toastStore.show({ message: failure(error, 'Could not choose this edition.'), type: 'error' });
		}
	}

	async function letChoose(): Promise<void> {
		try {
			await handBack.mutateAsync({ userId: userId(), localId: copyId, rgMbid: groupMbid });
			toastStore.show({
				message: 'DroppedNeedle will pick the edition that fits your files best.',
				type: 'success'
			});
			onchanged?.();
			dialog?.close();
		} catch (error) {
			toastStore.show({
				message: failure(error, 'Could not hand the edition back.'),
				type: 'error'
			});
		}
	}

	async function takeBack(): Promise<void> {
		try {
			await undo.mutateAsync({ userId: userId(), localId: copyId, rgMbid: groupMbid });
			toastStore.show({ message: 'Your last edition change was taken back.', type: 'success' });
			result = null;
			onchanged?.();
		} catch (error) {
			toastStore.show({ message: failure(error, 'Could not undo the change.'), type: 'error' });
		}
	}

	async function updateTags(): Promise<void> {
		if (!result) return;
		try {
			await retag.mutateAsync({
				userId: userId(),
				localId: copyId,
				rgMbid: groupMbid,
				files: result.retag_files
			});
			retagged = true;
			toastStore.show({ message: "The files now carry this edition's tags.", type: 'success' });
		} catch (error) {
			toastStore.show({ message: failure(error, 'Could not update the tags.'), type: 'error' });
		}
	}

	async function getMissing(): Promise<void> {
		if (!result) return;
		try {
			const answer = await acquire.mutateAsync({ mbid: result.release_group_mbid });
			acquired = true;
			toastStore.show({ message: answer.message, type: 'success' });
		} catch (error) {
			toastStore.show({
				message: failure(error, 'Could not request the missing tracks.'),
				type: 'error'
			});
		}
	}
</script>

<dialog bind:this={dialog} class="modal" aria-labelledby="edition-picker-title">
	<div class="modal-box flex max-h-[90vh] w-11/12 max-w-5xl flex-col gap-4">
		<header class="flex items-start justify-between gap-3">
			<div>
				<h2 id="edition-picker-title" class="flex items-center gap-2 text-xl font-bold">
					<Disc3 class="h-5 w-5 text-primary" /> Choose the edition
				</h2>
				<p class="mt-1 text-sm text-base-content/60">
					Pick the release your files are. It becomes this album's edition everywhere: pages, tags,
					other apps and downloads. Nothing automatic changes it afterwards.
				</p>
			</div>
			<form method="dialog">
				<button class="btn btn-ghost btn-sm btn-square" aria-label="Close"
					><X class="h-4 w-4" /></button
				>
			</form>
		</header>

		{#if copies.length > 1}
			<label class="form-control max-w-sm">
				<span class="label-text text-sm">Which copy?</span>
				<select class="select select-bordered select-sm mt-1" bind:value={copyId}>
					{#each copies as copy (copy.id)}<option value={copy.id}>{copy.title}</option>{/each}
				</select>
			</label>
		{/if}

		{#if result}
			<section class="rounded-box border border-success/30 bg-success/5 p-4" aria-live="polite">
				<p class="flex items-center gap-2 font-semibold">
					<Check class="h-4 w-4 text-success" /> Edition chosen
				</p>
				<p class="mt-1 text-sm text-base-content/70">
					{result.retag_files.length}
					{result.retag_files.length === 1 ? 'file sits' : 'files sit'} on this edition.
					{#if result.extra_track_ids.length}
						{result.extra_track_ids.length}
						{result.extra_track_ids.length === 1 ? 'file is' : 'files are'} not on it and keep their own
						tags.
					{/if}
					{#if result.missing_titles.length}
						{result.missing_titles.length} of its tracks are missing.
					{/if}
				</p>
				<div class="mt-3 flex flex-wrap gap-2">
					{#if result.retag_files.length}
						<button
							class="btn btn-primary btn-sm gap-2"
							disabled={retag.isPending || retagged}
							onclick={() => void updateTags()}
							><Tags class="h-4 w-4" />
							{retagged ? 'Tags updated' : "Write this edition's tags to the files"}</button
						>
					{/if}
					{#if result.missing_titles.length}
						<button
							class="btn btn-outline btn-sm gap-2"
							disabled={acquire.isPending || acquired}
							onclick={() => void getMissing()}
							><Download class="h-4 w-4" />
							{acquired ? 'Requested' : 'Get the missing tracks'}</button
						>
					{/if}
					<button
						class="btn btn-ghost btn-sm gap-2"
						disabled={undo.isPending}
						onclick={() => void takeBack()}><Undo2 class="h-4 w-4" /> Undo</button
					>
					<form method="dialog"><button class="btn btn-ghost btn-sm">Done</button></form>
				</div>
				<p class="mt-2 text-xs text-base-content/50">
					Other apps (Subsonic, Jellyfin) read the tags in your files, so they show the new edition
					once the tags are written.
				</p>
			</section>
		{:else}
			{#if status}
				<p class="text-sm text-base-content/70">
					<span class="font-medium">Now:</span>
					{status.reason.message}
				</p>
			{/if}
			<div class="grid min-h-0 flex-1 gap-4 md:grid-cols-[minmax(0,2fr)_minmax(0,3fr)]">
				<section class="flex min-h-0 flex-col gap-2" aria-label="Editions">
					<label class="input input-bordered input-sm flex items-center gap-2">
						<Search class="h-3.5 w-3.5 opacity-50" />
						<input
							class="grow"
							placeholder="Filter: year, country, CD, vinyl, barcode"
							bind:value={filter}
						/>
					</label>
					<ul class="min-h-0 flex-1 space-y-1 overflow-y-auto pr-1">
						{#if editionsQuery.isLoading}
							{#each Array(5) as _, index (index)}<li class="skeleton h-12"></li>{/each}
						{:else if editionsQuery.isError}
							<li class="text-sm text-base-content/55">
								MusicBrainz is not answering, so the editions can't be listed. Paste a release ID
								below instead.
							</li>
						{/if}
						{#each visible as edition (edition.release_mbid)}
							<li>
								<button
									type="button"
									class="flex w-full items-center justify-between gap-2 rounded-lg border px-3 py-2 text-left text-sm transition-colors {selected ===
									edition.release_mbid
										? 'border-primary bg-primary/10'
										: 'border-base-content/10 hover:border-primary/40'}"
									aria-pressed={selected === edition.release_mbid}
									onclick={() => (selected = edition.release_mbid)}
								>
									<span class="min-w-0">
										<span class="block truncate font-medium">{edition.title ?? 'Untitled'}</span>
										<span class="block truncate text-xs text-base-content/55">{label(edition)}</span
										>
									</span>
									<span class="flex shrink-0 flex-col items-end gap-1 text-xs">
										<span class="text-base-content/55">{edition.track_count} tracks</span>
										{#if edition.release_mbid === status?.release_mbid}
											<span class="badge badge-primary badge-xs">current</span>
										{/if}
									</span>
								</button>
							</li>
						{/each}
					</ul>
					<label class="form-control">
						<span class="label-text text-xs text-base-content/60"
							>Not listed? Paste any MusicBrainz release link or ID, even from another album</span
						>
						<input
							class="input input-bordered input-sm mt-1"
							placeholder="https://musicbrainz.org/release/…"
							bind:value={pasted}
							oninput={() => pastedMbid && (selected = pastedMbid.toLowerCase())}
						/>
					</label>
				</section>

				<section class="flex min-h-0 flex-col gap-2" aria-label="Tracklists side by side">
					<div class="flex items-center justify-between text-sm">
						<span class="font-medium">Your files and this edition</span>
						{#if selected && editionTracks.length}
							<span class="text-xs text-base-content/60"
								>{fits} of {files.length} files line up · {editionTracks.length} tracks on the edition</span
							>
						{/if}
					</div>
					{#if !selected}
						<p class="text-sm text-base-content/55">
							Pick an edition to compare its tracklist with your files.
						</p>
					{:else if tracksQuery.isLoading}
						<div class="skeleton h-40"></div>
					{:else if tracksQuery.isError}
						<p class="text-sm text-base-content/55">This edition's tracklist couldn't be loaded.</p>
					{:else}
						<div class="min-h-0 flex-1 overflow-y-auto rounded-box border border-base-content/10">
							<table class="table table-xs">
								<thead><tr><th>#</th><th>Your file</th><th>On this edition</th></tr></thead>
								<tbody>
									{#each rows as row (row.key)}
										<tr class={row.same ? '' : 'bg-warning/5'}>
											<td class="text-base-content/45">{row.key.replace('-', '.')}</td>
											<td class={row.file ? '' : 'text-base-content/35'}>{row.file ?? 'missing'}</td
											>
											<td class={row.edition ? '' : 'text-base-content/35'}>
												{row.edition ?? 'not on this edition'}
											</td>
										</tr>
									{/each}
								</tbody>
							</table>
						</div>
					{/if}
				</section>
			</div>

			<footer class="flex flex-wrap items-center justify-between gap-2">
				<div class="flex gap-2">
					{#if status?.state === 'chosen'}
						<button
							class="btn btn-ghost btn-sm"
							disabled={handBack.isPending}
							onclick={() => void letChoose()}>Let DroppedNeedle choose</button
						>
					{/if}
					{#if status?.undo_available}
						<button
							class="btn btn-ghost btn-sm gap-2"
							disabled={undo.isPending}
							onclick={() => void takeBack()}><Undo2 class="h-4 w-4" /> Undo last change</button
						>
					{/if}
				</div>
				<div class="flex gap-2">
					<form method="dialog"><button class="btn btn-ghost btn-sm">Cancel</button></form>
					<button
						class="btn btn-primary btn-sm"
						disabled={!selected || !copyId || choose.isPending}
						onclick={() => selected && void useEdition(selected)}
						>{choose.isPending ? 'Checking the files…' : 'Use this edition'}</button
					>
				</div>
			</footer>
		{/if}
	</div>
	<form method="dialog" class="modal-backdrop"><button>close</button></form>
</dialog>
