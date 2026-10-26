<script module lang="ts">
	// Only one preview plays at a time - starting one pauses whatever was playing, so two
	// files never overlap. Separate from the main player: listening never touches the queue.
	let activePreview: HTMLAudioElement | null = null;
	function claimPreview(el: HTMLAudioElement): void {
		if (activePreview && activePreview !== el) activePreview.pause();
		activePreview = el;
	}
</script>

<script lang="ts">
	import type { Snippet } from 'svelte';
	import { Check, Disc3, Pause, Play, RotateCcw, Trash2 } from 'lucide-svelte';

	import { getApiUrl } from '$lib/api/api-utils';
	import { HELD_ENDPOINTS } from '$lib/queries/downloads/endpoints';
	import {
		discardHeldMutation,
		importHeldMutation,
		recheckHeldMutation
	} from '$lib/queries/downloads/HeldQueries.svelte';
	import { formatCountdown } from '$lib/queries/downloads/downloadStatus';
	import { createFromAlbumMutation } from '$lib/queries/requests/RequestMutations.svelte';
	import type { HeldEntry, HeldFile } from '$lib/types';
	import Button from '$lib/ui/Button.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	import { keptUntil } from './heldExpiry';

	interface Props {
		entry: HeldEntry;
		file: HeldFile;
		/** False where the row sits under its own track, which already names it. */
		showTitle?: boolean;
		/** True where no entry card above says how long the file is kept. */
		showExpiry?: boolean;
		/** More actions, placed before Discard. */
		children?: Snippet;
	}

	let { entry, file, showTitle = true, showExpiry = false, children }: Props = $props();

	const importMut = importHeldMutation();
	const recheckMut = recheckHeldMutation();
	const discardMut = discardHeldMutation();
	const fromAlbum = createFromAlbumMutation();

	// Latched once the file has gone: the row stays mounted until the list refetches,
	// so without this a fast second click would act on a file that is no longer held.
	let done = $state(false);
	const busy = $derived(
		importMut.isPending ||
			recheckMut.isPending ||
			discardMut.isPending ||
			fromAlbum.isPending ||
			done ||
			entry.state !== 'held'
	);

	const input = $derived({
		entryId: entry.id,
		fileIds: [file.id],
		releaseGroupMbid: entry.release_group_mbid
	});
	const recording = $derived(file.recording_mbid ?? entry.recording_mbid ?? null);
	const offers = $derived(new Set(file.actions));

	const title = $derived(
		file.track_title
			? `${file.track_number ? `${file.track_number}. ` : ''}${file.track_title}`
			: file.original_name
	);

	// What the check that held it saw, so the person decides informed.
	const evidence = $derived.by(() => {
		const heard = file.evidence_title?.trim();
		const artist = file.evidence_artist?.trim();
		if (file.reason === 'target_occupied') {
			const detail = file.reason_detail?.trim().replace(/\.+$/, '');
			return detail ? `The file in the way: ${detail}` : null;
		}
		if (!heard && !artist) return null;
		const by = artist ? ` by ${artist}` : '';
		if (file.reason === 'tag_mismatch') return `Its tags say “${heard ?? 'unknown'}”${by}`;
		if (file.reason === 'wrong_track') return `Tagged “${heard ?? 'unknown'}”${by}`;
		const pct = file.evidence_score != null ? ` (${Math.round(file.evidence_score * 100)}%)` : '';
		return `AcoustID heard “${heard ?? 'a different recording'}”${by}${pct}`;
	});

	// -- listen: hear whether it is really the right song --
	let audioEl = $state<HTMLAudioElement | null>(null);
	let sourced = $state(false); // src attached on first play or seek, never eagerly
	let playing = $state(false);
	let currentTime = $state(0);
	let mediaDuration = $state(0);
	const total = $derived(mediaDuration || file.duration_seconds || 0);
	let failed = $state(false);

	function ensureSource(): void {
		if (audioEl && !sourced) {
			audioEl.src = getApiUrl(HELD_ENDPOINTS.audio(file.id));
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

	const meta = $derived(
		[
			file.expected_duration_seconds != null && total > 0
				? `Length ${fmt(total)} · expected ${fmt(file.expected_duration_seconds)}`
				: null,
			showExpiry ? keptUntil(entry.expires_at) : null
		]
			.filter(Boolean)
			.join(' · ')
	);

	const settle = { onSuccess: () => (done = true) };
	const settleIfGone = {
		onSuccess: (result: { still_held: number }) => {
			if (result.still_held === 0) done = true;
		}
	};

	// "Use this version" and "Use this file" put this file in place of a better one
	// you already have, so they ask first.
	let replaceOpen = $state(false);
	const replaceLabel = $derived(
		offers.has('use_this_version') ? 'Use this version' : 'Use this file'
	);

	async function replaceYours(): Promise<void> {
		const result = await importMut.mutateAsync(input);
		if (result.still_held === 0) done = true;
	}
</script>

<div class="space-y-2">
	{#if showTitle}
		<p class="truncate text-sm font-semibold" title={file.original_name}>{title}</p>
	{/if}
	<p class="text-xs text-base-content/60">
		{file.reason_text}{#if evidence}{` ${evidence}.`}{/if}
	</p>
	{#if file.reason_action}
		<p class="text-xs text-base-content/70">{file.reason_action}</p>
	{/if}
	{#if meta}
		<p class="text-[11px] tabular-nums text-base-content/50">{meta}</p>
	{/if}

	<div class="flex items-center gap-2">
		<IconButton
			size="sm"
			icon={playing ? Pause : Play}
			label={playing ? 'Pause' : 'Listen'}
			class="shrink-0"
			onclick={togglePlay}
			disabled={failed}
		/>
		<input
			type="range"
			class="held-seek h-1 min-w-0 flex-1"
			min="0"
			max={total || 1}
			step="0.1"
			value={currentTime}
			oninput={seek}
			disabled={failed}
			aria-label="Scrub {title}"
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
		<p class="text-[11px] text-base-content/45">Can't play this one here.</p>
	{/if}

	<div class="flex flex-wrap items-center gap-1.5">
		{#if offers.has('import')}
			<Button
				variant="primary"
				size="xs"
				icon={Check}
				onclick={() => importMut.mutate(input, settleIfGone)}
				disabled={busy}
			>
				Import
			</Button>
		{/if}
		{#if offers.has('retry')}
			<Button
				variant="primary"
				size="xs"
				icon={RotateCcw}
				onclick={() => importMut.mutate(input, settleIfGone)}
				disabled={busy}
			>
				Retry
			</Button>
		{/if}
		{#if offers.has('use_this_version') || offers.has('use_this_file')}
			<Button
				variant="primary"
				size="xs"
				icon={Check}
				onclick={() => (replaceOpen = true)}
				disabled={busy}
			>
				{replaceLabel}
			</Button>
		{/if}
		{#if offers.has('get_from_album') && recording}
			<Button
				variant="primary"
				size="xs"
				icon={Disc3}
				onclick={() => fromAlbum.mutate(recording)}
				disabled={busy}
			>
				Get it from the album
			</Button>
		{/if}
		{#if offers.has('recheck')}
			<Button
				variant="ghost"
				size="xs"
				icon={RotateCcw}
				loading={recheckMut.isPending}
				onclick={() => recheckMut.mutate(input, settleIfGone)}
				disabled={busy}
			>
				Re-check
			</Button>
		{/if}
		{@render children?.()}
		<Button
			variant="ghost"
			size="xs"
			icon={Trash2}
			onclick={() => discardMut.mutate(input, settle)}
			disabled={busy}
			aria-label="Discard {title}"
		>
			Discard
		</Button>
	</div>
</div>

<ConfirmDialog
	bind:open={replaceOpen}
	title="{replaceLabel}?"
	message="Yours goes to the recycle bin."
	confirmLabel={replaceLabel}
	pending={importMut.isPending}
	onconfirm={replaceYours}
/>

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
