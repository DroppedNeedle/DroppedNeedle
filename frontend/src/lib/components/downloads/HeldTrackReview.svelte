<script module lang="ts">
	// Only one preview plays at a time - starting one pauses whatever was playing, so two
	// tracks never overlap. Separate from the main player: auditioning never touches the queue.
	let activePreview: HTMLAudioElement | null = null;
	function claimPreview(el: HTMLAudioElement): void {
		if (activePreview && activePreview !== el) activePreview.pause();
		activePreview = el;
	}
</script>

<script lang="ts">
	import { Check, Pause, Play, X } from 'lucide-svelte';

	import { API } from '$lib/constants';
	import {
		discardHeldTrack,
		importHeldTrack
	} from '$lib/queries/downloads/DownloadMutations.svelte';
	import { formatCountdown } from '$lib/queries/downloads/downloadStatus';
	import type { HeldImport } from '$lib/types';

	let { held }: { held: HeldImport } = $props();

	const importMut = importHeldTrack();
	const discardMut = discardHeldTrack();
	// latch once resolved: the card stays mounted until the held query refetches, so
	// without this a fast second click re-POSTs an already-consumed held id
	let done = $state(false);
	const busy = $derived(importMut.isPending || discardMut.isPending || done);

	// what the rejecting check SAW - the reason we couldn't auto-confirm it, so the human
	// decides informed. The evidence source depends on the hold reason: AcoustID's
	// identification (fingerprint_mismatch), the file's own tags (tag_mismatch), or the
	// measured length vs the expected recording (wrong_track).
	const evidence = $derived.by(() => {
		const title = held.evidence_title?.trim();
		const artist = held.evidence_artist?.trim();
		if (held.reason === 'wrong_track') {
			const tagged = title ? `, tagged “${title}”${artist ? ` by ${artist}` : ''}` : '';
			return `Every source had the wrong length for this recording. This is the closest copy${tagged}`;
		}
		if (!title && !artist) return null;
		const by = artist ? ` by ${artist}` : '';
		if (held.reason === 'tag_mismatch') {
			return `The file's own tags say “${title ?? 'unknown'}”${by}`;
		}
		const pct = held.evidence_score != null ? ` (${Math.round(held.evidence_score * 100)}%)` : '';
		return `AcoustID heard “${title ?? 'a different recording'}”${by}${pct}`;
	});

	// -- inline audition: drop the needle to hear whether it's really the right song --
	let audioEl = $state<HTMLAudioElement | null>(null);
	let sourced = $state(false); // src attached lazily, only on first play/seek (no eager streaming)
	let playing = $state(false);
	let currentTime = $state(0);
	let mediaDuration = $state(0); // the audio element's real duration, once it loads
	// show the metadata duration up front, swap to the media's own once known
	const total = $derived(mediaDuration || held.duration_seconds || 0);
	let failed = $state(false);

	function ensureSource(): void {
		if (audioEl && !sourced) {
			audioEl.src = API.downloads.heldAudio(held.id);
			sourced = true;
		}
	}

	function togglePlay(): void {
		if (!audioEl) return;
		ensureSource();
		if (playing) {
			audioEl.pause();
		} else {
			claimPreview(audioEl);
			void audioEl.play().catch(() => (failed = true));
		}
	}

	function seek(e: Event): void {
		const value = Number((e.currentTarget as HTMLInputElement).value);
		if (!audioEl || !Number.isFinite(value)) return;
		ensureSource();
		audioEl.currentTime = value;
		currentTime = value;
	}

	const fmt = (s: number) => formatCountdown(Number.isFinite(s) ? s : 0);
</script>

<div class="space-y-2">
	<p class="text-xs text-base-content/55">
		Downloaded, but couldn't confirm it's the right recording.{#if evidence}
			{evidence}.{/if}
	</p>

	<div class="flex items-center gap-2">
		<button
			type="button"
			class="grid size-7 shrink-0 place-items-center rounded-full text-warning ring-1 ring-warning/30 transition-colors hover:bg-warning/10 disabled:opacity-40 motion-reduce:transition-none"
			onclick={togglePlay}
			disabled={failed}
			aria-label={playing ? 'Pause preview' : 'Play a preview to check the audio'}
		>
			{#if playing}<Pause class="h-3.5 w-3.5" />{:else}<Play class="h-3.5 w-3.5" />{/if}
		</button>
		<input
			type="range"
			class="held-seek h-1 flex-1"
			min="0"
			max={total || 1}
			step="0.1"
			value={currentTime}
			oninput={seek}
			disabled={failed}
			aria-label="Scrub preview"
		/>
		<span class="shrink-0 text-[11px] tabular-nums text-base-content/50">
			{fmt(currentTime)} / {fmt(total)}
		</span>
		<audio
			bind:this={audioEl}
			preload="none"
			onplay={() => (playing = true)}
			onpause={() => (playing = false)}
			onended={() => {
				playing = false;
				currentTime = 0;
			}}
			ontimeupdate={() => (currentTime = audioEl?.currentTime ?? 0)}
			onloadedmetadata={() => {
				const d = audioEl?.duration;
				if (d && Number.isFinite(d)) mediaDuration = d;
			}}
			onerror={() => (failed = true)}
		></audio>
	</div>
	{#if failed}
		<p class="text-[11px] text-base-content/45">
			Can't play this one in the browser. Decide from the details above.
		</p>
	{/if}

	<div class="flex flex-wrap items-center gap-1.5">
		<button
			class="btn btn-primary btn-xs"
			onclick={() =>
				importMut.mutate(
					{ id: held.id, release_group_mbid: held.release_group_mbid },
					{ onSuccess: () => (done = true) }
				)}
			disabled={busy}
			title="Add this file to your library anyway"
		>
			<Check class="h-3.5 w-3.5" /> Import anyway
		</button>
		<button
			class="btn btn-ghost btn-xs text-base-content/60 hover:text-error"
			onclick={() =>
				discardMut.mutate(
					{ id: held.id, release_group_mbid: held.release_group_mbid },
					{ onSuccess: () => (done = true) }
				)}
			disabled={busy}
			title="Delete this file and keep looking"
		>
			<X class="h-3.5 w-3.5" /> Discard
		</button>
	</div>
</div>

<style>
	.held-seek {
		accent-color: oklch(from var(--color-warning) l c h);
		cursor: pointer;
	}
	.held-seek:disabled {
		cursor: not-allowed;
		opacity: 0.5;
	}
</style>
