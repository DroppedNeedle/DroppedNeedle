<script lang="ts" module>
	export type FormatMode = 'with' | 'without';
	/** `atLeast` keeps the bound and above; `below` keeps what is under it. */
	export type TracksMode = 'atLeast' | 'below';
	/** `atLeast` keeps the stars and more; `atMost` the stars and fewer. */
	export type RatingMode = 'atLeast' | 'atMost';
</script>

<script lang="ts">
	// The library lists' filter button: a track count bound, the formats to
	// keep (With) or hide (Without), each with its count, and the caller's
	// star rating. Each category has its own direction. The menu changes the
	// filters as it goes; the list follows. A category is shown only when
	// the page passes its handler. On the album list the button reads as
	// the format select it leads with.
	import { Check, Layers, ListFilter, Star, X } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Popover from '$lib/ui/Popover.svelte';
	import type { components } from '$lib/api/v3/openapi';

	type FormatCount = components['schemas']['AlbumFormatCount'];

	interface Props {
		/** The track bound; 1 with `atLeast` keeps everything. */
		minTracks?: number;
		tracksMode?: TracksMode;
		onMinTracks?: (min: number, mode: TracksMode) => void;
		/** What the track count counts, for the heading. */
		tracksLabel?: string;
		/** The list's formats; absent on lists without a format filter. */
		formatCounts?: FormatCount[];
		/** What each format's count counts. */
		countLabel?: 'albums' | 'tracks';
		formats?: string[];
		formatMode?: FormatMode;
		onFormats?: (formats: string[], mode: FormatMode) => void;
		/** The caller's stars, 0 for any. */
		rating?: number;
		ratingMode?: RatingMode;
		onRating?: (stars: number, mode: RatingMode) => void;
	}

	let {
		minTracks = 1,
		tracksMode = 'atLeast',
		onMinTracks = undefined,
		tracksLabel = 'Tracks you have',
		formatCounts = undefined,
		countLabel = 'albums',
		formats = [],
		formatMode = 'with',
		onFormats = undefined,
		rating = 0,
		ratingMode = 'atLeast',
		onRating = undefined
	}: Props = $props();

	const PRESETS = [1, 2, 3, 5, 10];
	const MAX = 20;
	const STARS = [1, 2, 3, 4, 5];
	const sliderId = $props.id();

	const active = $derived(
		(onMinTracks && minTracks > 1 ? 1 : 0) +
			(onFormats && formats.length > 0 ? 1 : 0) +
			(onRating && rating > 0 ? 1 : 0)
	);
	const label = (min: number) =>
		tracksMode === 'below' ? `< ${min}` : min <= 1 ? 'Any' : `${min}+`;
	const buttonLabel = $derived.by(() => {
		if (!formatCounts || !onFormats) return 'Filter';
		if (formats.length === 0) return 'All formats';
		const list = formats.map((format) => format.toUpperCase()).join(', ');
		return formatMode === 'without' ? `Without ${list}` : list;
	});

	function toggleFormat(format: string) {
		const next = formats.includes(format)
			? formats.filter((known) => known !== format)
			: [...formats, format];
		onFormats?.(next, formatMode);
	}

	function reset() {
		onMinTracks?.(1, 'atLeast');
		onFormats?.([], 'with');
		onRating?.(0, 'atLeast');
	}
</script>

{#snippet modes(name: string, current: string, choices: string[][], pick: (mode: string) => void)}
	<div class="join" role="radiogroup" aria-label={name}>
		{#each choices as [mode, text] (mode)}
			<Button
				variant={current === mode ? 'primary' : 'ghost'}
				size="xs"
				active={current === mode}
				class="join-item"
				role="radio"
				aria-checked={current === mode}
				onclick={() => pick(mode)}>{text}</Button
			>
		{/each}
	</div>
{/snippet}

<Popover
	label={buttonLabel}
	title="Filters"
	icon={formatCounts && onFormats ? Layers : ListFilter}
	caret={Boolean(formatCounts && onFormats)}
	count={active}
	class={formatCounts && onFormats ? 'sm:w-58' : ''}
>
	{#snippet children(close)}
		{#if onMinTracks}
			<div class="space-y-2">
				<div class="flex items-center justify-between">
					<label class="text-sm font-semibold" for={sliderId}>{tracksLabel}</label>
					{@render modes(
						'Track filter mode',
						tracksMode,
						[
							['atLeast', 'At least'],
							['below', 'Fewer than']
						],
						(mode) =>
							onMinTracks(mode === 'below' ? Math.max(minTracks, 2) : minTracks, mode as TracksMode)
					)}
				</div>
				<input
					id={sliderId}
					type="range"
					min={tracksMode === 'below' ? 2 : 1}
					max={MAX}
					class="range range-sm range-primary"
					value={Math.min(minTracks, MAX)}
					aria-valuetext={label(minTracks)}
					onchange={(event) =>
						onMinTracks(Number((event.target as HTMLInputElement).value), tracksMode)}
				/>
				<div class="flex flex-wrap gap-1">
					{#each PRESETS.filter((preset) => tracksMode === 'atLeast' || preset > 1) as preset (preset)}
						<Button
							variant={minTracks === preset ? 'primary' : 'ghost'}
							size="xs"
							active={minTracks === preset}
							aria-pressed={minTracks === preset}
							onclick={() => onMinTracks(preset, tracksMode)}>{label(preset)}</Button
						>
					{/each}
					{#if !PRESETS.includes(minTracks)}
						<span class="badge badge-primary badge-sm self-center">{label(minTracks)}</span>
					{/if}
				</div>
			</div>
		{/if}

		{#if formatCounts && onFormats}
			{#if onMinTracks}<div class="divider my-0"></div>{/if}
			<div class="space-y-2">
				<div class="flex items-center justify-between">
					<p class="text-sm font-semibold">Formats</p>
					{@render modes(
						'Format filter mode',
						formatMode,
						[
							['with', 'With'],
							['without', 'Without']
						],
						(mode) => onFormats(formats, mode as FormatMode)
					)}
				</div>
				<div class="flex flex-wrap gap-2">
					{#each formatCounts as { format, album_count } (format)}
						{@const marked = formats.includes(format)}
						<Button
							variant={marked ? (formatMode === 'without' ? 'danger' : 'primary') : 'outline'}
							size="sm"
							icon={marked ? (formatMode === 'without' ? X : Check) : undefined}
							active={marked}
							aria-pressed={marked}
							onclick={() => toggleFormat(format)}
							>{format.toUpperCase()}
							<span class="font-normal opacity-60" title="{album_count} {countLabel}"
								>{album_count}</span
							></Button
						>
					{/each}
				</div>
				<p class="text-xs text-muted">
					{formatMode === 'without'
						? 'Hides the marked formats.'
						: 'Shows only the marked formats.'}
				</p>
			</div>
		{/if}

		{#if onRating}
			{#if onMinTracks || (formatCounts && onFormats)}<div class="divider my-0"></div>{/if}
			<div class="space-y-2">
				<div class="flex items-center justify-between">
					<p class="text-sm font-semibold">Your rating</p>
					{@render modes(
						'Rating filter mode',
						ratingMode,
						[
							['atLeast', 'At least'],
							['atMost', 'At most']
						],
						(mode) => onRating(rating, mode as RatingMode)
					)}
				</div>
				<div class="flex items-center gap-1">
					<Button
						variant={rating === 0 ? 'primary' : 'ghost'}
						size="xs"
						active={rating === 0}
						aria-pressed={rating === 0}
						onclick={() => onRating(0, ratingMode)}>Any</Button
					>
					{#each STARS as star (star)}
						{@const lit =
							rating > 0 && (ratingMode === 'atLeast' ? star >= rating : star <= rating)}
						<IconButton
							icon={Star}
							shape="square"
							size="sm"
							label={star === 1 ? '1 star' : `${star} stars`}
							aria-pressed={star === rating}
							class="[&_svg]:size-5 {lit
								? '[&_svg]:fill-warning [&_svg]:text-warning'
								: '[&_svg]:text-base-content/30'}"
							onclick={() => onRating(star, ratingMode)}
						/>
					{/each}
				</div>
				<p class="text-xs text-base-content/60">
					{rating === 0
						? 'Rated or not.'
						: ratingMode === 'atLeast'
							? `Only what you rated ${rating} star${rating === 1 ? '' : 's'} or more.`
							: `Only what you rated ${rating} star${rating === 1 ? '' : 's'} or fewer.`}
				</p>
			</div>
		{/if}

		<div class="flex justify-between pt-1">
			<Button variant="ghost" size="xs" onclick={reset} disabled={!active}>Reset</Button>
			<Button variant="primary" size="xs" onclick={close}>Done</Button>
		</div>
	{/snippet}
</Popover>
