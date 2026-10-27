<script lang="ts">
	import { Pause, Play, Trash2, TriangleAlert, X } from 'lucide-svelte';

	import { getApiUrl } from '$lib/api/api-utils';
	import { gatewayStreamUrl } from '$lib/player/playbackGateway';
	import { removeLibraryTrack } from '$lib/queries/library/LibraryMutations.svelte';
	import { LibraryQueryKeyFactory } from '$lib/queries/library/LibraryQueryKeyFactory';
	import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
	import { formatCountdown } from '$lib/queries/downloads/downloadStatus';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { toastStore } from '$lib/stores/toast';
	import type { UnmatchedFile } from './albumEdition';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	interface Props {
		files: UnmatchedFile[];
		albumMbid: string;
		/** The local album the files sit under. */
		albumId: string;
		canRemove?: boolean;
	}

	let { files, albumMbid, albumId, canRemove = false }: Props = $props();

	const remove = removeLibraryTrack();
	// two-step inline confirm: first click arms THIS row, second click removes
	let confirmingId = $state<string | null>(null);

	function handleRemove(file: UnmatchedFile): void {
		if (confirmingId !== file.id) {
			confirmingId = file.id;
			return;
		}
		confirmingId = null;
		if (activeId === file.id) stopPreview();
		remove.mutate(
			{ fileId: file.id, albumMbid },
			{
				onSuccess: () => {
					void invalidateQueriesWithPersister({
						queryKey: LibraryQueryKeyFactory.catalog.editionPlanPrefix(authStore.user?.id, albumId)
					});
					toastStore.show({ message: 'File removed', type: 'success' });
				},
				onError: () => toastStore.show({ message: "Couldn't remove this file", type: 'error' })
			}
		);
	}

	// -- inline audition: ONE shared element for the section (only one preview plays
	// at a time), src attached lazily so listing orphans never streams anything, and
	// auditioning never touches the play queue.
	let audioEl = $state<HTMLAudioElement | null>(null);
	let activeId = $state<string | null>(null);
	let playing = $state(false);
	let currentTime = $state(0);
	let mediaDuration = $state(0);
	let failed = $state(false);

	function stopPreview(): void {
		audioEl?.pause();
		activeId = null;
		playing = false;
		currentTime = 0;
		mediaDuration = 0;
	}

	function togglePreview(file: UnmatchedFile): void {
		if (!audioEl) return;
		failed = false;
		if (activeId === file.id) {
			if (playing) audioEl.pause();
			else void audioEl.play().catch(() => (failed = true));
			return;
		}
		activeId = file.id;
		currentTime = 0;
		mediaDuration = 0;
		audioEl.src = getApiUrl(gatewayStreamUrl('local', file.id));
		void audioEl.play().catch(() => (failed = true));
	}

	function seek(e: Event): void {
		const value = Number((e.currentTarget as HTMLInputElement).value);
		if (!audioEl || !Number.isFinite(value)) return;
		audioEl.currentTime = value;
		currentTime = value;
	}

	const fmt = (s: number) => formatCountdown(Number.isFinite(s) ? s : 0);
	const seconds = (file: UnmatchedFile) => (file.lengthMs ? file.lengthMs / 1000 : 0);
</script>

{#if files.length > 0}
	<Card as="section" padding="md" class="border-l-4 border-warning" aria-label="Unmatched files">
		<div class="flex flex-wrap items-center gap-2">
			<TriangleAlert class="h-4 w-4 text-warning" />
			<h2 class="text-sm font-bold">Unmatched files</h2>
			<span class="badge badge-sm badge-warning">{files.length}</span>
			<p class="w-full text-xs opacity-60 sm:w-auto sm:ml-1">
				Stored under this album, but not on any track of its edition.
			</p>
		</div>

		<ul class="divide-y divide-base-300">
			{#each files as file (file.id)}
				<li class="py-2.5">
					<div class="flex flex-wrap items-center gap-3">
						<IconButton
							size="sm"
							icon={activeId === file.id && playing ? Pause : Play}
							label={activeId === file.id && playing ? 'Pause preview' : 'Play a preview'}
							onclick={() => togglePreview(file)}
						/>
						<div class="min-w-0 flex-1">
							<p class="truncate text-sm font-medium">
								{file.artistName ? `${file.artistName} - ` : ''}{file.title}
							</p>
							<p class="truncate text-xs opacity-60">
								{file.format?.toUpperCase() ?? '?'}
								{#if file.lengthMs}
									• {fmt(seconds(file))}
								{/if}
							</p>
							{#if file.note}
								<p class="text-xs text-info">{file.note}</p>
							{/if}
						</div>
						<span class="badge badge-sm badge-warning badge-outline whitespace-nowrap">
							{file.label}
						</span>
						{#if canRemove}
							{#if confirmingId === file.id}
								<div class="flex items-center gap-1">
									<Button
										variant="danger"
										size="xs"
										icon={Trash2}
										onclick={() => handleRemove(file)}
										disabled={remove.isPending}
									>
										Remove file
									</Button>
									<IconButton
										size="xs"
										shape="square"
										icon={X}
										label="Keep this file"
										onclick={() => (confirmingId = null)}
									/>
								</div>
							{:else}
								<Button
									variant="danger-ghost"
									size="xs"
									icon={Trash2}
									onclick={() => handleRemove(file)}
									disabled={remove.isPending}
								>
									Remove
								</Button>
							{/if}
						{/if}
					</div>
					{#if activeId === file.id}
						<div class="mt-1.5 flex items-center gap-2 pl-11 pr-1">
							<input
								type="range"
								class="range range-xs range-warning flex-1"
								aria-label="Scrub preview"
								min="0"
								max={mediaDuration || seconds(file)}
								step="1"
								value={currentTime}
								oninput={seek}
							/>
							<span class="text-xs tabular-nums opacity-60">
								{fmt(currentTime)} / {fmt(mediaDuration || seconds(file))}
							</span>
						</div>
						{#if failed}
							<p class="pl-11 text-xs text-error">Couldn't play this file.</p>
						{/if}
					{/if}
				</li>
			{/each}
		</ul>
	</Card>

	<audio
		bind:this={audioEl}
		preload="none"
		onplay={() => (playing = true)}
		onpause={() => (playing = false)}
		onended={stopPreview}
		ontimeupdate={() => (currentTime = audioEl?.currentTime ?? 0)}
		onloadedmetadata={() => (mediaDuration = audioEl?.duration ?? 0)}
	></audio>
{/if}
