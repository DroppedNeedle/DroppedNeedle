<script lang="ts">
	import { Pause, Play, Trash2, TriangleAlert, X } from 'lucide-svelte';

	import { API } from '$lib/constants';
	import { removeLibraryTrack } from '$lib/queries/library/LibraryMutations.svelte';
	import { formatCountdown } from '$lib/queries/downloads/downloadStatus';
	import { toastStore } from '$lib/stores/toast';
	import type { LibraryTrack } from '$lib/types';

	interface Props {
		orphans: LibraryTrack[];
		albumMbid: string;
		canRemove?: boolean;
	}

	let { orphans, albumMbid, canRemove = false }: Props = $props();

	const remove = removeLibraryTrack();
	// two-step inline confirm: first click arms THIS row, second click removes
	let confirmingId = $state<string | null>(null);

	function handleRemove(file: LibraryTrack): void {
		if (confirmingId !== file.id) {
			confirmingId = file.id;
			return;
		}
		confirmingId = null;
		if (activeId === file.id) stopPreview();
		remove.mutate(
			{ fileId: file.id, albumMbid },
			{
				onSuccess: () => toastStore.show({ message: 'File removed', type: 'success' }),
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

	function togglePreview(file: LibraryTrack): void {
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
		audioEl.src = API.stream.local(file.id);
		void audioEl.play().catch(() => (failed = true));
	}

	function seek(e: Event): void {
		const value = Number((e.currentTarget as HTMLInputElement).value);
		if (!audioEl || !Number.isFinite(value)) return;
		audioEl.currentTime = value;
		currentTime = value;
	}

	function basename(path: string): string {
		return path.replace(/\\/g, '/').split('/').pop() ?? path;
	}

	const fmt = (s: number) => formatCountdown(Number.isFinite(s) ? s : 0);
</script>

{#if orphans.length > 0}
	<section class="rounded-2xl border border-warning/25 bg-base-200/50">
		<div class="flex flex-col gap-3 p-4 sm:p-5">
			<div class="flex flex-wrap items-center gap-2.5">
				<h2
					class="flex items-center gap-2.5 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
				>
					<TriangleAlert class="h-4 w-4 text-warning" />
					Unmatched files
				</h2>
				<span class="badge badge-sm badge-warning badge-outline font-mono tabular-nums"
					>{orphans.length}</span
				>
				<p class="w-full text-xs text-base-content/55 sm:w-auto sm:ml-1">
					Stored under this album, but they don't match any of its tracks.
				</p>
			</div>

			<ul class="divide-y divide-base-content/8">
				{#each orphans as file (file.id)}
					<li class="py-2.5">
						<div class="flex flex-wrap items-center gap-3">
							<button
								class="btn btn-sm btn-circle btn-ghost"
								aria-label={activeId === file.id && playing ? 'Pause preview' : 'Play a preview'}
								onclick={() => togglePreview(file)}
							>
								{#if activeId === file.id && playing}
									<Pause class="h-4 w-4" />
								{:else}
									<Play class="h-4 w-4" />
								{/if}
							</button>
							<div class="min-w-0 flex-1">
								<p class="truncate text-sm font-medium">
									{file.artist_name ? `${file.artist_name} - ` : ''}{file.track_title ||
										basename(file.file_path)}
								</p>
								<p class="truncate font-mono text-[0.68rem] tabular-nums text-base-content/50">
									{file.file_format?.toUpperCase() ?? '?'}
									{#if file.duration_seconds}
										• {fmt(file.duration_seconds)}
									{/if}
									• {basename(file.file_path)}
								</p>
							</div>
							<span
								class="badge badge-sm badge-warning badge-outline whitespace-nowrap rounded-full"
							>
								Doesn't match
							</span>
							{#if canRemove}
								{#if confirmingId === file.id}
									<div class="flex items-center gap-1">
										<button
											class="btn btn-xs btn-error gap-1"
											onclick={() => handleRemove(file)}
											disabled={remove.isPending}
										>
											<Trash2 class="h-3.5 w-3.5" />
											Remove file
										</button>
										<button
											class="btn btn-xs btn-ghost"
											aria-label="Keep this file"
											onclick={() => (confirmingId = null)}
										>
											<X class="h-3.5 w-3.5" />
										</button>
									</div>
								{:else}
									<button
										class="btn btn-xs btn-ghost text-error gap-1"
										onclick={() => handleRemove(file)}
										disabled={remove.isPending}
									>
										<Trash2 class="h-3.5 w-3.5" />
										Remove
									</button>
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
									max={mediaDuration || file.duration_seconds || 0}
									step="1"
									value={currentTime}
									oninput={seek}
								/>
								<span class="font-mono text-xs tabular-nums text-base-content/50">
									{fmt(currentTime)} / {fmt(mediaDuration || file.duration_seconds || 0)}
								</span>
							</div>
							{#if failed}
								<p class="pl-11 text-xs text-error">Couldn't play this file.</p>
							{/if}
						{/if}
					</li>
				{/each}
			</ul>
		</div>
	</section>

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
