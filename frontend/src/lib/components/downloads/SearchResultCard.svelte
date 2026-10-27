<script lang="ts" module>
	import type { CandidateRefusal } from '$lib/queries/downloads/SearchQueries.svelte';

	/** Why a candidate is outside the download settings, as a short label. */
	export const REFUSAL_LABELS: Record<CandidateRefusal, string> = {
		quality_outside_range: 'Quality outside your range',
		over_size_cap: 'Over your size limit',
		ignored_term: 'Has a term you ignore',
		required_term_missing: 'Missing a term you require',
		below_minimum_score: 'Scored below your minimum'
	};
</script>

<script lang="ts">
	import {
		BadgeCheck,
		CircleAlert,
		Disc3,
		Download,
		Files,
		Gauge,
		Library,
		Puzzle,
		Signal,
		Timer
	} from 'lucide-svelte';

	import type { SearchCandidate } from '$lib/queries/downloads/SearchQueries.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';

	interface Props {
		candidate: SearchCandidate;
		onPick?: () => void;
		picking?: boolean;
		/** lock every pick button once a pick is in flight or committed (double-pick guard) */
		disabled?: boolean;
	}
	const { candidate, onPick, picking = false, disabled = false }: Props = $props();

	const isSoulseek = $derived(candidate.source === 'soulseek');
	const isUsenet = $derived(candidate.source === 'usenet');
	const recommended = $derived(candidate.tier === 'recommended');
	const refusal = $derived(candidate.refusal?.[0] ?? null);
	const noteTone = $derived(
		recommended ? 'text-success' : refusal ? 'text-base-content/60' : 'text-warning'
	);
	const scoreLabel = $derived(
		candidate.score != null ? `score ${Math.round(candidate.score * 100)}%` : null
	);
	const lengthsLabel = $derived(
		candidate.length_misses
			? `${candidate.length_misses} ${candidate.length_misses === 1 ? 'length' : 'lengths'} off`
			: null
	);

	function sizeLabel(bytes: number): string {
		if (bytes >= 1024 ** 3) return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
		if (bytes >= 1024 ** 2) return `${Math.round(bytes / 1024 ** 2)} MB`;
		return `${Math.max(0, Math.round(bytes / 1024))} KB`;
	}

	const ageLabel = $derived.by(() => {
		if (!candidate.posted_at) return '';
		const days = Math.floor((Date.now() / 1000 - candidate.posted_at) / 86400);
		if (days <= 0) return 'today';
		return days >= 30 ? `${Math.floor(days / 30)}mo` : `${days}d`;
	});

	const tracksLabel = $derived(
		candidate.tracks_matched != null && candidate.tracks_total != null
			? `${candidate.tracks_matched}/${candidate.tracks_total} tracks`
			: candidate.file_count > 0
				? `${candidate.file_count} ${candidate.file_count === 1 ? 'file' : 'files'}`
				: null
	);
	const subtitle = $derived(
		isUsenet ? (candidate.indexer ?? 'Usenet') : (candidate.username ?? candidate.source)
	);
	const fileList = $derived(
		candidate.files
			.slice(0, 20)
			.map((file) => file.filename.split(/[\\/]/).pop())
			.join('\n')
	);
</script>

<Card as="article" variant="bordered" padding="sm" class="sleeve-card">
	<div class="flex items-center gap-3 sm:gap-4">
		<div
			class="sleeve hidden size-14 shrink-0 place-items-center rounded-md bg-base-300 sm:grid"
			aria-hidden="true"
		>
			{#if isSoulseek}
				<Disc3 class="size-7 text-base-content/60" />
			{:else if isUsenet}
				<Download class="size-7 text-base-content/60" />
			{:else}
				<Puzzle class="size-7 text-base-content/60" />
			{/if}
		</div>

		<div class="min-w-0 flex-1">
			<p class="truncate text-base font-semibold" title={candidate.title}>{candidate.title}</p>
			<p class="truncate text-sm text-base-content/60" title={subtitle}>{subtitle}</p>
			{#if refusal}
				<p
					class="mt-1 flex items-center gap-1 text-xs font-medium text-error"
					data-testid="candidate-refusal"
				>
					<CircleAlert class="size-3.5 shrink-0" aria-hidden="true" />
					{REFUSAL_LABELS[refusal]}
				</p>
			{/if}
			<p class="mt-1 text-xs {noteTone}" data-testid="candidate-note">
				{candidate.note.text}
			</p>
			<div class="mt-1.5 flex flex-wrap items-center gap-1.5">
				{#if candidate.format}
					<span
						class="badge badge-sm"
						class:badge-success={recommended}
						class:badge-error={refusal === 'quality_outside_range'}
						class:badge-outline={refusal === 'quality_outside_range'}>{candidate.format}</span
					>
				{/if}
				{#if scoreLabel}
					<span
						class="badge badge-ghost badge-sm gap-1"
						title="The source's own match score for this release"
					>
						<Gauge class="size-3" aria-hidden="true" />{scoreLabel}
					</span>
				{/if}
				{#if tracksLabel}
					<span class="badge badge-ghost badge-sm gap-1" title={fileList || undefined}>
						<Files class="size-3" aria-hidden="true" />{tracksLabel}
					</span>
				{/if}
				{#if lengthsLabel}
					<span class="badge badge-ghost badge-sm gap-1 text-warning">
						<Timer class="size-3" aria-hidden="true" />{lengthsLabel}
					</span>
				{/if}
				{#if candidate.size_bytes > 0}
					<span class="badge badge-ghost badge-sm">{sizeLabel(candidate.size_bytes)}</span>
				{/if}
				{#if isUsenet}
					{#if candidate.indexer}
						<span class="badge badge-ghost badge-sm gap-1">
							<Library class="size-3" aria-hidden="true" />{candidate.indexer}
						</span>
					{/if}
					{#if candidate.grabs}
						<span class="badge badge-ghost badge-sm gap-1" aria-label="Grabs">
							<Signal class="size-3" aria-hidden="true" />{candidate.grabs}
						</span>
					{/if}
					{#if ageLabel}<span class="badge badge-ghost badge-sm">{ageLabel}</span>{/if}
				{/if}
				{#if isSoulseek}
					{#if candidate.upload_speed}
						<span class="badge badge-ghost badge-sm gap-1" aria-label="Upload speed">
							<Signal class="size-3" aria-hidden="true" />{Math.round(
								candidate.upload_speed / 1000
							)} KB/s
						</span>
					{/if}
					{#if candidate.has_free_slot}
						<span
							class="badge badge-ghost badge-sm gap-1 text-success"
							aria-label="Free slot available"
						>
							<BadgeCheck class="size-3" aria-hidden="true" />slot
						</span>
					{:else if candidate.queue_length}
						<span class="badge badge-ghost badge-sm">{candidate.queue_length} queued</span>
					{/if}
				{/if}
			</div>
		</div>

		<Button
			variant={recommended ? 'primary' : 'outline'}
			size="sm"
			class="min-h-11 shrink-0"
			loading={picking}
			{disabled}
			onclick={onPick}
			aria-label={`Pick ${candidate.title} from ${subtitle}`}
		>
			{recommended ? 'Pick' : refusal ? 'Pick anyway…' : 'Pick anyway'}
		</Button>
	</div>
</Card>

<style>
	/* the sleeve tilt rides on the kit Card, outside this component's scope */
	:global(.sleeve-card) {
		transform: perspective(900px) rotateY(-3deg);
		transform-style: preserve-3d;
		transition:
			transform 0.35s ease,
			box-shadow 0.35s ease;
		animation: fade-in-up 0.3s ease both;
	}
	:global(.sleeve-card:hover) {
		transform: perspective(900px) rotateY(0deg) translateY(-2px);
		box-shadow: 0 12px 30px oklch(from var(--color-base-300) l c h / 0.6);
	}
	.sleeve {
		transform: translateZ(20px) rotateY(6deg);
		box-shadow: 4px 4px 12px oklch(from var(--color-base-300) l c h / 0.7);
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
		:global(.sleeve-card) {
			animation: none;
			transition: none;
		}
		:global(.sleeve-card:hover) {
			transform: none;
			box-shadow: none;
		}
	}
</style>
