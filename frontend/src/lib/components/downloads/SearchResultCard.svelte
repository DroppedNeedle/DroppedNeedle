<script lang="ts">
	import { BadgeCheck, Disc3, Download, Files, Library, Magnet, Signal } from 'lucide-svelte';

	import type { ScoredCandidate } from '$lib/types';

	interface Props {
		candidate: ScoredCandidate;
		albumTitle?: string;
		viaAlbumNzb?: boolean;
		onPick?: () => void;
		picking?: boolean;
		/** lock every pick button once a pick is in flight / already committed (double-pick guard) */
		disabled?: boolean;
	}
	const {
		candidate,
		albumTitle,
		viaAlbumNzb = false,
		onPick,
		picking = false,
		disabled = false
	}: Props = $props();

	const RING_R = 26;
	const RING_C = 2 * Math.PI * RING_R;

	const isUsenet = $derived(candidate.source === 'usenet' && Boolean(candidate.usenet_release));
	const isTorrent = $derived(candidate.source === 'torrent' && Boolean(candidate.torrent_release));
	const percent = $derived(Math.round(candidate.final_score * 100));

	// --- soulseek signals ---
	const fileCount = $derived(candidate.files.length);
	const freeSlot = $derived(candidate.files.some((f) => f.has_free_slot));
	const uploadSpeed = $derived(Math.max(0, ...candidate.files.map((f) => f.upload_speed)));
	const soulseekFormat = $derived.by(() => {
		const ext = (candidate.files[0]?.extension ?? '').toUpperCase();
		const bitrate = Math.max(0, ...candidate.files.map((f) => f.bitrate ?? 0));
		if (!ext) return 'AUDIO';
		return bitrate && !['FLAC', 'ALAC', 'WAV', 'APE', 'WV'].includes(ext)
			? `${ext} ${bitrate}`
			: ext;
	});

	// --- usenet signals (from the release: category is the reliable quality signal) ---
	const rel = $derived(isTorrent ? candidate.torrent_release : candidate.usenet_release);
	const usenetFormat = $derived.by(() => {
		const cats = rel?.category_ids ?? [];
		if (cats.includes(3040)) return 'FLAC';
		if (cats.includes(3010)) return 'MP3';
		const t = (rel?.title ?? '').toUpperCase();
		if (/\bFLAC|24BIT|LOSSLESS\b/.test(t)) return 'FLAC';
		if (/\b320\b/.test(t)) return 'MP3 320';
		return 'unknown'; // don't fake a bitrate when the title doesn't parse
	});
	const sizeLabel = $derived.by(() => {
		const b = rel?.size_bytes ?? 0;
		if (b >= 1024 ** 3) return `${(b / 1024 ** 3).toFixed(1)} GB`;
		return `${Math.round(b / 1024 ** 2)} MB`;
	});
	const ageLabel = $derived.by(() => {
		const published = isTorrent
			? candidate.torrent_release?.publish_date
			: candidate.usenet_release?.usenet_date;
		if (!published) return '';
		const days = Math.floor((Date.now() / 1000 - published) / 86400);
		if (days <= 0) return 'today';
		return days >= 30 ? `${Math.floor(days / 30)}mo` : `${days}d`;
	});

	const tierClass = $derived(
		candidate.tier === 'auto'
			? 'ring-accent text-accent'
			: candidate.tier === 'manual'
				? 'ring-warning text-warning'
				: 'ring-base-content/30 text-base-content/50'
	);

	const breakdown = $derived(
		isUsenet || isTorrent
			? `${rel?.indexer_name ?? (isTorrent ? 'Torrent' : 'Usenet')} · ${usenetFormat} · ${sizeLabel}` +
					`${isTorrent && rel && 'seeders' in rel ? ` · ${rel.seeders ?? 'unknown'} seeders` : rel?.grabs ? ` · ${rel.grabs} grabs` : ''}${ageLabel ? ` · ${ageLabel}` : ''}`
			: `Coherence ${Math.round(candidate.coherence * 100)}% · ` +
					`File confidence ${Math.round(candidate.file_confidence * 100)}% · ` +
					`${freeSlot ? 'Free slot' : 'Queued'}${uploadSpeed ? ` · ${Math.round(uploadSpeed / 1000)} KB/s` : ''}`
	);

	const heading = $derived(
		isUsenet || isTorrent
			? albumTitle || rel?.title || 'Unknown'
			: candidate.parent_directory || 'Unknown folder'
	);
	const subtitle = $derived(isUsenet || isTorrent ? (rel?.title ?? '') : candidate.username);

	const dashoffset = $derived(RING_C * (1 - Math.max(0, Math.min(1, candidate.final_score))));
</script>

<div class="sleeve-card flex items-center gap-4 rounded-box border border-base-300 bg-base-200 p-3">
	<div
		class="sleeve grid size-14 shrink-0 place-items-center rounded-md bg-base-300"
		aria-hidden="true"
	>
		{#if isTorrent}
			<Magnet class="size-7 text-base-content/60" />
		{:else if isUsenet}
			<Download class="size-7 text-base-content/60" />
		{:else}
			<Disc3 class="size-7 text-base-content/60" />
		{/if}
	</div>

	<div class="min-w-0 flex-1">
		<p class="truncate font-semibold" title={heading}>{heading}</p>
		<p class="truncate text-sm text-base-content/60" title={subtitle}>{subtitle}</p>
		<div class="mt-1.5 flex flex-wrap items-center gap-1.5">
			{#if isUsenet || isTorrent}
				<span class="badge badge-ghost badge-sm gap-1">
					<Library class="size-3" aria-hidden="true" />{rel?.indexer_name}
				</span>
				<span
					class="badge badge-sm"
					class:badge-success={usenetFormat !== 'unknown' && candidate.tier !== 'rejected'}
					>{usenetFormat}</span
				>
				<span class="badge badge-ghost badge-sm">{sizeLabel}</span>
				{#if isTorrent && rel && 'seeders' in rel}
					<span class="badge badge-ghost badge-sm gap-1" aria-label="Seeders">
						<Signal class="size-3" aria-hidden="true" />{rel.seeders ?? 'unknown'}
					</span>
				{:else if rel?.grabs}
					<span class="badge badge-ghost badge-sm gap-1" aria-label="Grabs">
						<Signal class="size-3" aria-hidden="true" />{rel.grabs}
					</span>
				{/if}
				{#if ageLabel}<span class="badge badge-ghost badge-sm">{ageLabel}</span>{/if}
				{#if isUsenet && viaAlbumNzb}
					<span class="badge badge-ghost badge-sm" title="Grabs the album NZB to extract one track"
						>via album NZB</span
					>
				{/if}
			{:else}
				<span class="badge badge-sm" class:badge-success={candidate.tier !== 'rejected'}
					>{soulseekFormat}</span
				>
				<span class="badge badge-ghost badge-sm gap-1">
					<Files class="size-3" aria-hidden="true" />{fileCount}
					{fileCount === 1 ? 'track' : 'tracks'}
				</span>
				{#if uploadSpeed > 0}
					<span class="badge badge-ghost badge-sm gap-1" aria-label="Has upload speed">
						<Signal class="size-3" aria-hidden="true" />{Math.round(uploadSpeed / 1000)} KB/s
					</span>
				{/if}
				{#if freeSlot}
					<span
						class="badge badge-ghost badge-sm gap-1 text-success"
						aria-label="Free slot available"
					>
						<BadgeCheck class="size-3" aria-hidden="true" />slot
					</span>
				{/if}
			{/if}
		</div>
	</div>

	<div class="tooltip tooltip-left shrink-0" data-tip={breakdown}>
		<div class={`score-ring grid size-[64px] place-items-center rounded-full ring-1 ${tierClass}`}>
			<svg viewBox="0 0 64 64" class="absolute size-[64px] -rotate-90">
				<circle
					cx="32"
					cy="32"
					r={RING_R}
					fill="none"
					stroke="currentColor"
					stroke-width="4"
					class="opacity-15"
				/>
				<circle
					class="ring-progress"
					cx="32"
					cy="32"
					r={RING_R}
					fill="none"
					stroke="currentColor"
					stroke-width="4"
					stroke-linecap="round"
					stroke-dasharray={RING_C}
					stroke-dashoffset={dashoffset}
				/>
			</svg>
			<span class="text-sm font-bold tabular-nums">{percent}%</span>
		</div>
	</div>

	<button
		type="button"
		class="btn btn-primary btn-sm shrink-0"
		onclick={onPick}
		disabled={picking || disabled}
		aria-label={isUsenet || isTorrent
			? `Pick release from ${rel?.indexer_name}`
			: `Pick candidate from ${candidate.username}`}
	>
		{#if picking}<span class="loading loading-spinner loading-xs"></span>{/if}
		Pick
	</button>
</div>

<style>
	.sleeve-card {
		transform: perspective(900px) rotateY(-3deg);
		transform-style: preserve-3d;
		transition:
			transform 0.35s ease,
			box-shadow 0.35s ease;
		animation: fade-in-up 0.3s ease both;
	}
	.sleeve-card:hover {
		transform: perspective(900px) rotateY(0deg) translateY(-2px);
		box-shadow: 0 12px 30px oklch(from var(--color-base-300) l c h / 0.6);
	}
	.sleeve {
		transform: translateZ(20px) rotateY(6deg);
		box-shadow: 4px 4px 12px oklch(from var(--color-base-300) l c h / 0.7);
	}
	.score-ring {
		position: relative;
	}
	/* from-only keyframe animates to the element's own stroke-dashoffset */
	.ring-progress {
		animation: ring-fill 0.9s cubic-bezier(0.4, 0, 0.2, 1) both;
	}
	@keyframes ring-fill {
		from {
			stroke-dashoffset: 170;
		}
	}
	@keyframes fade-in-up {
		0% {
			opacity: 0;
			transform: perspective(900px) rotateY(-3deg) translateY(10px);
		}
		100% {
			opacity: 1;
			transform: perspective(900px) rotateY(-3deg) translateY(0);
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.sleeve-card,
		.ring-progress {
			animation: none;
		}
	}
</style>
